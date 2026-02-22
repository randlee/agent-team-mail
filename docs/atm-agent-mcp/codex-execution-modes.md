# Codex Execution Modes: A Complete Reference

> A practical guide to all four ways to run Codex, with a focus on the lesser-known `app-server` mode.

ATM mapping note:
- In `atm-agent-mcp` config, `transport = "cli-json"` maps to `codex exec --json`.
- `transport = "mcp"` maps to `codex mcp-server`.
- `transport = "app-server"` maps to `codex app-server`.

Related docs:
- `docs/atm-agent-mcp/requirements.md`
- `docs/atm-agent-mcp/codex-mcp-crate-design.md`
- `docs/atm-agent-mcp/app-server-protocol-reference.md`
- `docs/codex-json-schema.md`

---

## Overview

Codex has four distinct execution modes, each designed for a different integration pattern. Most documentation emphasizes the first two; the latter two are powerful but underexposed.

| Mode | Command | Transport | Primary Use Case |
|---|---|---|---|
| Interactive TUI | `codex` | Terminal UI | Human-in-the-loop development |
| Non-interactive | `codex exec` | stdout / JSONL | CI, scripts, single-turn automation |
| MCP server | `codex mcp-server` | stdio (MCP protocol) | Codex as a tool inside another agent |
| **App server** | `codex app-server` | stdio (JSON-RPC 2.0 / JSONL) | Embedding Codex inside your own product |

---

## Mode 1: Interactive TUI

```bash
codex
codex "explain this codebase"
codex --json   # machine-readable event stream output
```

The default mode. Launches a full-screen terminal UI where a human sends prompts and reviews Codex's actions in real time. Supports approval flows, inline command execution, image input, and session history.

Adding `--json` emits newline-delimited JSON events to stdout — useful for observation, but injecting messages back into the session via stdin is fragile since the TUI was designed for keyboard input, not programmatic writes.

---

## Mode 2: Non-interactive (`codex exec`)

```bash
codex exec "generate release notes for the last 10 commits"
codex exec --json "triage open bug reports"
codex exec --ephemeral "review this PR for race conditions"
```

**Analogous to Claude Code's `-p` flag**: one prompt in, the agent works autonomously until done, final message printed to stdout, then exit. No human interaction expected.

Key flags:
- `--json` — emit full JSONL event stream (turn started/completed, items, tool calls, etc.)
- `--ephemeral` — don't persist session history to disk
- `--sandbox` — set sandbox policy (`read-only`, `workspace-write`, `danger-full-access`)
- `--output-last-message <path>` / `-o` — write the final agent message to a file

Sessions can be resumed across multiple `exec` calls:

```bash
codex exec "review this change for race conditions"
codex exec resume --last "now fix the race conditions you found"
```

Ideal for CI pipelines, pre-merge checks, and scripted workflows.

---

## Mode 3: MCP Server

```bash
codex mcp-server
```

Runs Codex as an [MCP (Model Context Protocol)](https://modelcontextprotocol.io/) server over stdio. Another agent or MCP client connects to it and can invoke Codex as a tool — Codex becomes a callable capability, not the top-level orchestrator.

Use this when:
- You have an outer agent (Claude, another Codex, a custom harness) that needs to delegate coding tasks
- You want to compose Codex into a multi-agent pipeline where it's one node among many

This is a pure tool-server model. The outer agent sends tool calls; Codex executes them and returns results. There's no persistent conversation state managed by the caller — that's Codex's internal concern.

---

## Mode 4: App Server *(the hidden gem)*

```bash
codex app-server
```

**This is the protocol the Codex VS Code extension uses internally.** OpenAI has published it as a first-class API, but it's largely undocumented in user-facing materials and easy to miss even after reading the codebase thoroughly.

### What it is

A long-running process that speaks **JSON-RPC 2.0 over stdio** (JSONL, one message per line). It's bidirectional: your program writes requests to its stdin, reads streaming notifications and responses from its stdout. Think of it as the "engine" behind any rich Codex client.

Unlike MCP mode where Codex is a passive tool, in app-server mode **you** are the client building the outer shell — with full control over threads, turns, streaming, approvals, and session state.

### Protocol basics

Messages omit the standard `"jsonrpc":"2.0"` header. Three message types:

**Request** (client → server):
```json
{ "method": "turn/start", "id": 2, "params": { "threadId": "thr_123", "input": [{"type": "text", "text": "Refactor this module"}] } }
```

**Response** (server → client):
```json
{ "id": 2, "result": { "turn": { "id": "turn_456" } } }
```

**Notification** (server → client, no `id`):
```json
{ "method": "item/agentMessage/delta", "params": { "delta": "Here is my plan..." } }
```

### Schema generation

Generate typed schemas for your specific installed version:

```bash
codex app-server generate-ts --out ./schemas        # TypeScript
codex app-server generate-json-schema --out ./schemas  # JSON Schema
```

### Core primitives

- **Thread** — a conversation between a user and the Codex agent; persisted across sessions
- **Turn** — a single user request and all agent work that follows; streams incremental items
- **Item** — a unit of input or output: agent message, command execution, file change, tool call, web search, plan update, etc.

### Lifecycle

```
1. Launch:  codex app-server
2. Send:    initialize  →  initialized
3. Send:    thread/start  (or thread/resume / thread/fork)
4. Send:    turn/start  with user input
5. Read:    stream of item/* and turn/* notifications
6. Read:    turn/completed
7. Repeat steps 4-6 for subsequent turns
```

### Minimal Node.js client example

```typescript
import { spawn } from "node:child_process";
import readline from "node:readline";

const proc = spawn("codex", ["app-server"], {
  stdio: ["pipe", "pipe", "inherit"],
});
const rl = readline.createInterface({ input: proc.stdout });

const send = (msg: unknown) => proc.stdin.write(`${JSON.stringify(msg)}\n`);

let threadId: string | null = null;

rl.on("line", (line) => {
  const msg = JSON.parse(line);

  if (msg.id === 1 && msg.result?.thread?.id && !threadId) {
    threadId = msg.result.thread.id;
    send({
      method: "turn/start",
      id: 2,
      params: {
        threadId,
        input: [{ type: "text", text: "Summarize this repo." }],
      },
    });
  }
});

// Handshake
send({ method: "initialize", id: 0, params: { clientInfo: { name: "my_app", version: "1.0.0" } } });
send({ method: "initialized", params: {} });

// Start thread
send({ method: "thread/start", id: 1, params: { model: "gpt-5.1-codex", cwd: process.cwd() } });
```

### Key API methods

**Thread management:**
| Method | Description |
|---|---|
| `thread/start` | Create a new conversation thread |
| `thread/resume` | Reopen an existing thread by ID |
| `thread/fork` | Branch a thread into a new thread ID |
| `thread/rollback` | Drop the last N turns from context |
| `thread/read` | Read stored thread data without resuming |
| `thread/list` | Paginate through stored threads |
| `thread/archive` / `thread/unarchive` | Archive management |

**Turn control:**
| Method | Description |
|---|---|
| `turn/start` | Submit user input and begin agent generation |
| `turn/steer` | **Inject a message into an in-flight turn** (key for programmatic use) |
| `turn/interrupt` | Cancel the current turn mid-stream |

**Streaming notifications** (server → client):
| Notification | Description |
|---|---|
| `item/started` | A new unit of work has begun |
| `item/completed` | Final state of a completed item |
| `item/agentMessage/delta` | Streamed text from the agent |
| `item/commandExecution/outputDelta` | Live stdout/stderr from commands |
| `item/plan/delta` | Streamed plan text |
| `item/reasoning/summaryTextDelta` | Readable reasoning summaries |
| `turn/completed` | Turn is done; includes final status and usage |

**Other useful methods:**
| Method | Description |
|---|---|
| `command/exec` | Run a shell command under the sandbox directly, no agent involved |
| `review/start` | Start a Codex code review for a thread |
| `model/list` | List available models |
| `skills/list` | List available skills |
| `config/read` | Read effective resolved config |
| `config/value/write` | Write a config key to `config.toml` |

### `turn/steer` — the critical method for orchestration

This is what replaces the pattern of injecting raw bytes into a TUI's stdin. While a turn is in flight, you can call:

```json
{ "method": "turn/steer", "id": 5, "params": { "threadId": "thr_123", "input": [{"type": "text", "text": "Actually, focus only on the auth module"}] } }
```

The agent receives this as additional user context mid-generation. For multi-agent systems, this is the clean primitive for routing messages to a running Codex instance.

### Approval handling

Unlike `codex exec` where you preset approval policy upfront, app-server streams approval-request events to your client. You can programmatically approve or reject — giving you full control over what the agent is allowed to do without the human-facing approval UI.

---

## Choosing the Right Mode

```
Do you need a human at the keyboard?
  └─ Yes → Interactive TUI (codex)

Is this a single automated task (CI, script)?
  └─ Yes → Non-interactive (codex exec)

Is Codex one tool among many in an agent pipeline?
  └─ Yes → MCP server (codex mcp-server)

Are you building a product/integration that controls Codex programmatically,
needs streaming output, persistent threads, or bidirectional communication?
  └─ Yes → App server (codex app-server)
```

---

## The `notify` Hook and Its Limitations

All modes share the `notify` config option in `~/.codex/config.toml`:

```toml
notify = ["bash", "-lc", "my-script.sh"]
```

Key facts about `notify`:
- Currently fires only on **`agent-turn-complete`** events
- **Synchronous** — Codex waits for the process to exit before continuing. Long-running calls will block the session.
- **No timeout** — a hung hook hangs Codex indefinitely
- **Stdout is ignored** — you cannot return a message that Codex will act on
- Self-detach any slow work: `do_slow_thing "$1" &` and return immediately

For true bidirectional hook behavior (inject messages, steer the agent, react to tool use), **app-server is the answer** — `notify` is intentionally a side-channel notification only.

---

## Source

- Official docs: https://developers.openai.com/codex/app-server/
- Open source implementation: https://github.com/openai/codex/tree/main/codex-rs/app-server
- Config reference: https://developers.openai.com/codex/config-reference/
