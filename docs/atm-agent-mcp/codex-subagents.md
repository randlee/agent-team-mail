# Codex Native Subagent Tools

As of February 2026, Codex has native multi-agent tools built directly into its runtime (`codex-rs/core/src/tools/handlers/multi_agents.rs`). These are not MCP tools — they are first-class function calls injected into your context automatically.

You do not need any external setup. If these tools appear in your available functions, you can use them now.

---

## Agent Roles

Three built-in roles are available via `agent_type`:

| Role | Purpose |
|---|---|
| `worker` | Implementation tasks — writing code, fixing bugs, refactoring. Give workers explicit file/responsibility ownership. Always tell them they are not alone in the codebase. |
| `explorer` | Fast read-only codebase questions. Run in parallel. Trust results without re-verification. |
| `default` | General purpose. |

---

## Tools

### `spawn_agent`

Spawn a new subagent with an initial prompt.

```json
{
  "message": "Implement the authentication middleware in src/auth.ts",
  "agent_type": "worker"
}
```

```json
{
  "message": "Where is the database connection pool configured?",
  "agent_type": "explorer"
}
```

**Returns:** `{ "agent_id": "string" }` — save this to interact with the agent later.

**Constraints:**
- There is a maximum spawn depth. If exceeded, you will receive: `"Agent depth limit reached. Solve the task yourself."`
- Use `message` for simple text prompts. Use `items` for structured multi-part input.

---

### `wait`

Block until one or more agents finish (or timeout).

```json
{
  "ids": ["agent-id-1", "agent-id-2"],
  "timeout_ms": 30000
}
```

**Returns:** `{ "status": { "<id>": "completed" | "running" | ... }, "timed_out": bool }`

**Constraints:**
- `timeout_ms` is clamped between **10,000ms** (10s) and **300,000ms** (5min). Do not use short timeouts to poll — it wastes CPU.
- Default timeout if omitted: 30,000ms.
- Pass multiple ids to wait for parallel workers simultaneously.

---

### `send_input`

Send a follow-up message to a running agent, optionally interrupting it first.

```json
{
  "id": "agent-id-1",
  "message": "Also add unit tests for the new middleware.",
  "interrupt": false
}
```

```json
{
  "id": "agent-id-1",
  "message": "Stop what you're doing. Use the existing Logger class instead.",
  "interrupt": true
}
```

**Returns:** `{ "submission_id": "string" }`

- Set `interrupt: true` to cancel the agent's current work before sending the new message.
- Set `interrupt: false` (default) to queue the message after current work completes.

---

### `resume_agent`

Resume a paused or closed agent by id. Can restore agents from persistent rollout storage.

```json
{
  "id": "agent-id-1"
}
```

Useful for long-running tasks that were suspended or for picking up work from a previous session.

---

### `close_agent`

Terminate an agent when its work is done.

```json
{
  "id": "agent-id-1"
}
```

Always close agents when finished to free resources.

---

## Typical Orchestration Pattern

### Sequential task delegation

```
1. spawn_agent(message="...", agent_type="worker") → { agent_id: "w1" }
2. wait(ids=["w1"])
3. close_agent(id="w1")
```

### Parallel workers

```
1. spawn_agent(message="Implement feature A", agent_type="worker") → { agent_id: "w1" }
2. spawn_agent(message="Implement feature B", agent_type="worker") → { agent_id: "w2" }
3. wait(ids=["w1", "w2"])          ← waits for both simultaneously
4. close_agent(id="w1")
5. close_agent(id="w2")
```

### Research then implement

```
1. spawn_agent(message="Find all places that handle auth tokens", agent_type="explorer") → { agent_id: "e1" }
2. wait(ids=["e1"])
3. close_agent(id="e1")
4. spawn_agent(message="Refactor auth token handling based on [explorer findings]", agent_type="worker") → { agent_id: "w1" }
5. wait(ids=["w1"])
6. close_agent(id="w1")
```

### Steering a running agent

```
1. spawn_agent(message="Refactor the payment module", agent_type="worker") → { agent_id: "w1" }
2. wait(ids=["w1"], timeout_ms=15000)   ← check progress
3. send_input(id="w1", message="Keep backward compatibility with v1 API")
4. wait(ids=["w1"])
5. close_agent(id="w1")
```

---

## Worker Instructions Best Practice

When spawning a `worker`, always tell it:
- Exactly which files or components it owns
- That other agents may be working in the same codebase concurrently — ignore changes made by others
- Not to use destructive git commands (`git reset --hard`, `git checkout --`) unless explicitly asked
- Not to amend commits unless explicitly asked

Example:
```json
{
  "agent_type": "worker",
  "message": "You own src/auth/. Implement JWT refresh token rotation. Other agents are working elsewhere in the codebase — do not touch their files."
}
```

---

## Additional Operational Guardrails (from Collaboration Prompting)

In addition to the worker prompt guidance above, current collaboration templates add a few practical rules:

- If multiple sub-agents run at once, explicitly remind each one it is not alone in the environment.
- For log-heavy tasks (tests/config commands), you can delegate to a sub-agent to keep main-context size down.
- In those delegated test/config cases, explicitly tell that sub-agent **not** to spawn further sub-agents (to avoid recursion).
- Sub-agents inherit the same tool access you have unless you constrain behavior in your prompt.

Reference: `prompts/templates/collab/experimental_prompt.md`

---

## Related Async Interface (MCP Sessions)

This document covers native subagent tools. A separate, complementary async path exists through Codex MCP:

- `codex` starts a session and returns a durable `threadId`
- `codex-reply` continues that same session later (including from another orchestrator/session)

This is useful for async orchestration patterns where you want to checkpoint, pause, or hand off long-running work outside a single foreground interaction.

Reference: `codex-mcp.md`

---

## Hooks Status (Current Repo Docs)

As of the docs in this repository, there is no separately documented hook registration framework (for example: lifecycle/plugin/tool callback hooks with payload schemas). Documented control surfaces are the native subagent tool calls and the MCP tool/session interfaces.

---

## Notes

- These tools are native to the Codex runtime as of v0.104+ (currently on `main`, shipping soon).
- They are **not** MCP tools — no external server required.
- Source: `codex-rs/core/src/tools/handlers/multi_agents.rs`
