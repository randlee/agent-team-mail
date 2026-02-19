# sc-atm-agent-mcp Design

> **Synaptic Canvas plugin spec** — end-user integration guide for running Codex as a Claude subagent via `atm-agent-mcp`. See `codex-mcp-crate-design.md` for the internal implementation blueprint.
>
> **Status**: Subordinate integration guide. Normative requirements live in `docs/atm-agent-mcp/requirements.md`.

Codex running as a Claude subagent via MCP, with access to native multi-agent tools and the `atm` CLI for cross-system communication with Claude agent teams.

---

## System Prompt

Combine the base Codex prompt with the multi-agent addendum:

**Base**: latest bundled Codex base prompt
**Addendum**: `prompts/templates/collab/experimental_prompt.md`

When using the MCP `codex` tool, pass via `developer-instructions` to augment without replacing the base prompt:

```json
{
  "name": "codex",
  "arguments": {
    "prompt": "...",
    "cwd": "/path/to/project",
    "sandbox": "workspace-write",
    "developer-instructions": "<contents of experimental_prompt.md>"
  }
}
```

Or use `base-instructions` to supply the full combined prompt:

```json
{
  "name": "codex",
  "arguments": {
    "prompt": "...",
    "base-instructions": "<codex_base_prompt>\n\n<experimental_prompt.md>"
  }
}
```

---

## Native Subagent Tools

When the `multi_agent` feature is enabled (`/experimental` → Multi-agents), Codex gains:

| Tool | Purpose |
|---|---|
| `spawn_agent` | Create a subagent (`worker`, `explorer`, `default`) |
| `wait` | Block until one or more agents finish |
| `send_input` | Send a follow-up message, optionally interrupting |
| `resume_agent` | Resume a paused/closed agent |
| `close_agent` | Terminate an agent |

See `codex-subagents.md` for full schemas and patterns.

---

## Agent Teams Mail (`atm`)

`atm` is a CLI for sending and receiving messages between Claude agent teams via `~/.claude/teams/`. This gives Codex a communication channel to and from Claude agent teams running elsewhere — including humans, CI systems, and other orchestrators.

### Installation

```bash
brew install agent-team-mail   # macOS/Linux
# or
cargo install agent-team-mail
```

### Key Commands

```bash
# Send a message to a Claude agent
atm send <agent-name> "message"
atm send <agent-name>@<team-name> "cross-team message"

# Read your inbox
atm read                        # unread only, marks as read
atm read --all --no-mark        # read all without marking

# Broadcast to a whole team
atm broadcast "message"
atm broadcast --team <team> "message"

# Discover teams and members
atm teams
atm members <team-name>
atm status
```

### Configuration

Set defaults via `.atm.toml` in the project root or `~/.config/atm/config.toml`:

```toml
[core]
default_team = "my-team"
identity = "codex"
```

Or via environment variables: `ATM_TEAM`, `ATM_IDENTITY`.

### Usage Patterns for Codex

**Report completion to a Claude orchestrator:**
```bash
atm send orchestrator@my-team "Task complete: refactored auth module. PR ready for review."
```

**Request clarification from a teammate:**
```bash
atm send team-lead@my-team "Ambiguous requirement in issue #42 — should the token TTL be per-user or global?"
atm read team-lead   # poll for reply
```

**Notify a CI agent:**
```bash
atm send ci-agent@my-team "Tests passing on feature/auth-refresh. Ready to merge."
```

**Check team status before starting:**
```bash
atm status
atm members my-team
```

---

## Combined Architecture

```
Claude (orchestrator)
  │
  ├─► atm-agent-mcp (MCP proxy)
  │     └─► codex mcp-server (single child process)
  │           └─► 0..N sessions (`agent_id` -> `threadId`)
  │           ├─► spawn_agent(worker) ──► worker subagent
  │           ├─► spawn_agent(explorer) ► explorer subagent
  │           └─► atm send/read ─────────► Claude agent teams
  │                                         (humans, CI, other orchestrators)
  │
  └─► threadId (durable) ──► codex-reply (resume later)
```

---

## MCP Server Setup

```bash
claude mcp add atm-agent-mcp -s user \
  -e PATH="/your/node/bin:/usr/local/bin:/usr/bin:/bin" \
  -- atm-agent-mcp serve
```

`atm-agent-mcp` manages the downstream `codex mcp-server` child process internally. Auth still uses `codex login` — no `OPENAI_API_KEY` env var needed if already authenticated.

See `codex-mcp.md` for Codex MCP tool reference (`codex` + `codex-reply`).

---

## Recommended `approval-policy` for Subagent Use

| Scenario | Policy |
|---|---|
| Fully automated pipeline | `never` |
| Automated with safety net | `on-failure` |
| Human in the loop via `atm` | `on-request` — Codex requests approval, human replies via `atm` |

---

## Notes

- Multi-agent tools require `multi_agent` feature enabled (`/experimental` in Codex TUI)
- Currently on `main`, shipping in v0.104+
- `atm` operates on `~/.claude/teams/` — Claude agent teams must be running on the same machine (or a shared filesystem) for direct delivery; cross-machine support is planned via `atm-daemon`
