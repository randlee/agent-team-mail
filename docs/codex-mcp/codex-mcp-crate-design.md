# `atm-agent-mcp` Crate Design

A new crate in the `agent-team-mail` workspace: `crates/atm-agent-mcp`.

A thin MCP proxy that wraps `codex mcp-server`, injects ATM identity/team/repo context, provides native ATM MCP tools, and manages 0..N Codex sessions through one proxy process.

---

## Workspace Integration

```
agent-team-mail/
├── crates/
│   ├── atm-core/       # existing — config, IO, schema
│   ├── atm/            # existing — CLI
│   ├── atm-daemon/     # existing — daemon
│   └── atm-agent-mcp/      # NEW — MCP proxy binary
```

Binary name: `atm-agent-mcp`

Dependencies: `atm-core`, `tokio`, `serde_json`, `rmcp` (or raw stdio MCP), `signal-hook`

---

## One-To-Many Session Model

`atm-agent-mcp` is a single proxy that manages one Codex child process and many logical agent sessions.

```
Claude MCP client
  -> atm-agent-mcp (single process)
       -> codex mcp-server (single child process)
       -> 0..N active sessions (agent_id -> codex threadId)
```

Each active session binds 1:1 to an ATM identity while active.
The MCP-facing session key is `agent_id` (backend-agnostic). Internally, Codex uses `threadId`.

Lifecycle summary:
1. Proxy starts and loads config/registry.
2. First `codex` request lazily starts child process if needed.
3. New sessions receive `agent_id` and identity binding.
4. Sessions move through `busy`/`idle`/`closed`.
5. Shutdown persists registry + summaries, then stops child process.

---

## Session Context

The proxy refreshes runtime context per turn and injects it into Codex requests. Session metadata is also persisted in the registry:

| Field | Source | Description |
|---|---|---|
| `identity` | config resolution | Agent's name on the team |
| `team` | `[core].default_team` | Team name |
| `repo_root` | git rev-parse --show-toplevel | Absolute path to git root, or `null` when outside git |
| `repo_name` | git remote name or directory name | Human-readable repo identifier, or `null` when outside git |
| `cwd` | process working directory | Launch directory (may differ from repo root) |
| `branch` | git rev-parse --abbrev-ref HEAD | Current branch at launch time |

`cwd` remains independent. It MUST NOT be reused as a fake repository identifier.

This context is injected on `codex` and `codex-reply` turns and stored in registry entries for `atm-agent-mcp sessions` and resume flows.

---

## Configuration

Agent-proxy config lives in `.atm.toml` under `[plugins.atm-agent-mcp]`:

```toml
[core]
default_team = "my-team"
identity = "codex"              # default identity if --identity not passed

[plugins.atm-agent-mcp]
# Path to codex binary (default: resolved from PATH)
codex_bin = "codex"

# Agent identity — overrides [core].identity for atm-agent-mcp sessions
identity = "codex-architect"

# Model override (optional). If omitted, no model is passed and Codex uses its latest default.
# model = "o3"

# Optional fast profile model (for --fast)
fast_model = "gpt-5.3-codex-spark"

# Reasoning effort (default: none)
reasoning_effort = "high"

# Sandbox mode (default: "workspace-write")
sandbox = "workspace-write"

# Approval policy (default: "on-failure")
approval_policy = "on-failure"

# Prompt files to inject (defaults are bundled prompts)
base_prompt_file = ""          # empty = use bundled
extra_instructions_file = ""   # empty = use bundled experimental_prompt.md

# Incoming mail polling interval when idle, in milliseconds (default: 5000)
mail_poll_interval_ms = 5000

# How long to wait with no new mail before stopping idle polling (default: 300000 = 5min)
idle_timeout_ms = 300000

# Persist active thread IDs to disk for resume across restarts (default: true)
persist_threads = true

# Named role presets (see Roles section below)
[plugins.atm-agent-mcp.roles.architect]
model = "o3"
reasoning_effort = "high"
sandbox = "read-only"

[plugins.atm-agent-mcp.roles.worker]
model = "gpt-5.3-codex"
sandbox = "workspace-write"
approval_policy = "never"
```

Config resolution follows the same priority chain as `atm`:
1. CLI flags (`--identity`, `--role`, `--model`, `--fast`, `--subagents`, `--readonly|--explore`, `--sandbox`, `--approval-policy`)
2. Environment variables (`ATM_AGENT_MCP_IDENTITY`, `ATM_AGENT_MCP_MODEL`, `ATM_AGENT_MCP_SANDBOX`, etc.)
3. `[plugins.atm-agent-mcp]` in repo-local `.atm.toml`
4. `[plugins.atm-agent-mcp]` in `~/.config/atm/config.toml`
5. Defaults

---

## Architecture

```
claude (MCP client)
    │  stdio
    ▼
atm-agent-mcp (proxy)
    │
    ├── on startup:
    │     resolve_config()          ← atm-core: reads .atm.toml, env, defaults
    │     detect_team_context()     ← team + launch cwd/repo facts
    │     load_prompts()            ← bundled prompts + optional extras
    │     load_session_registry()   ← restore persisted sessions from disk
    │
    ├── on first codex request:
    │     spawn codex mcp-server    ← lazy child process start
    │
    ├── MCP request intercept:
    │     tools/list      → pass through + add atm_send/atm_read/atm_broadcast
    │     codex           → resolve/assign agent_id + identity, inject context, forward
    │     codex-reply     → resolve agent_id->threadId, refresh context, forward
    │     atm_send        → handled locally via atm-core (no shell needed)
    │     atm_read        → handled locally via atm-core
    │     atm_broadcast   → handled locally via atm-core
    │     *               → pass through
    │
    ├── on codex/codex-reply response:
    │     extract threadId          ← update agent_id->threadId mapping + session metadata
    │     persist registry to disk
    │
    ├── idle detection:
    │     after turn completes, start mail poll timer
    │     every poll_interval_ms: atm-core inbox read for this identity
    │     if messages → format digest, inject as codex-reply on active thread
    │     if no active thread → start new codex session with mail as prompt
    │     if idle_timeout_ms expires with no mail → stop polling
    │     mail arriving during active turn → queue, deliver after turn completes
    │
    └── on shutdown (SIGTERM/SIGINT/parent disconnect):
          request summary from each active thread
          write summary files to disk
          persist final thread registry
          deregister from team
          terminate codex mcp-server child
```

---

## Thread Registry

Active sessions are tracked in memory and persisted to `~/.config/atm/agent-sessions/<team>/registry.json`:

```json
{
  "version": 1,
  "sessions": [
    {
      "agent_id": "codex:thread_abc123",
      "backend": "codex",
      "backend_id": "abc123",
      "identity": "codex-architect",
      "team": "my-team",
      "repo_root": "/Users/rand/projects/myapp",
      "repo_name": "myapp",
      "branch": "feature/auth-refresh",
      "cwd": "/Users/rand/projects/myapp/src",
      "started_at": "2026-02-17T10:00:00Z",
      "last_active": "2026-02-17T10:45:00Z",
      "status": "active",
      "tag": "feature/auth-refresh"
    }
  ]
}
```

One proxy instance is the only writer for its team-scoped registry file. Writes use `atm-core` atomic I/O.
Audit logs are written alongside it at `~/.config/atm/agent-sessions/<team>/audit.jsonl`.

**Shutdown behavior:**
- Graceful (SIGTERM): request summary from each own thread, write summary files, deregister, exit
- Forced (SIGKILL / parent disconnect): registry persists as-is for next session to resume from

---

## Prompt Injection

On `codex` and `codex-reply`, `atm-agent-mcp` injects context into `developer-instructions`:

```
<session-context>
Identity:  {identity}
Team:      {team}
Repo:      {repo_name} ({repo_root})
Branch:    {branch}
</session-context>

<orchestrator-communication>
ATM MCP tools (`atm_send`, `atm_read`, `atm_broadcast`) are available to the MCP client/orchestrator.
They are auditable alternatives to shelling out to `atm`.
</orchestrator-communication>

<multi-agent>
[contents of experimental_prompt.md]
</multi-agent>
```

The full `base-instructions` uses bundled prompts unless the caller supplies `base-instructions`; proxy context is still appended via `developer-instructions`.

---

## ATM as MCP Tools

The proxy exposes `atm_send`, `atm_read`, and `atm_broadcast` as first-class MCP tools, implemented directly via `atm-core` — no shell execution required:

### `atm_send`
```json
{
  "to": "team-lead@atm-dev",
  "message": "PR is ready for review.",
  "summary": "PR ready notification"
}
```

### `atm_read`
```json
{
  "mark_read": true
}
```
Returns array of messages with `from`, `content`, `received_at`.

### `atm_broadcast`
```json
{
  "message": "Pausing work — need input on auth approach.",
  "summary": "Requesting team input"
}
```

Benefits over shell `atm` commands: no approval policy friction, visible in MCP tool call logs, auditable by the orchestrator.

---

## Incoming Mail as Turns

When Codex becomes idle (a turn completes, no new call arrives within `mail_poll_interval_ms`):

1. Read inbox for this identity via `atm-core`
2. If unread messages exist, format a digest and inject as `codex-reply` on the active session:
   ```
   [Incoming mail]
   From: team-lead@atm-dev — "Can you add rate limiting to the auth endpoint?"
   From: ci-agent@my-team — "Tests failing on feature/auth-refresh: timeout in test_token_expiry"
   ```
3. Mark messages as read only after successful handoff to Codex
4. If no active session is bound to the identity — keep mail unread for later delivery
5. Resume polling after response completes

**Mail routing with multiple sessions:**
- Routing is deterministic: identity -> active session mapping.
- No heuristic routing by tag/branch.

---

## Session Summary and Resume

On graceful shutdown, `atm-agent-mcp` requests a compacted summary from each active session:

```
Session ending. Write a concise summary of:
- What you were working on
- Current state — what is done, what is not
- Any open questions or blockers
- Next steps if resumed
```

Written to: `~/.config/atm/agent-sessions/<team>/<identity>/<backend-id>/summary.md`

### Resuming with Context

```bash
# Resume most recent session for this identity
atm-agent-mcp serve --resume

# Resume a specific thread by ID
atm-agent-mcp serve --resume <agent-id>

# Resume by identity name (picks most recent session for that identity)
atm-agent-mcp serve --resume codex-architect
```

The summary is prepended to `developer-instructions` on the first turn of the new session:

```
[Previous session — {identity} on {repo_name}/{branch}]
<contents of summary.md>
[End of previous session]
```

Summary files are kept until `atm-agent-mcp sessions --prune` or overwritten at next shutdown.

---

## CLI Interface

```bash
# Start the MCP server
atm-agent-mcp serve
atm-agent-mcp serve --identity codex-worker-1
atm-agent-mcp serve --role worker
atm-agent-mcp serve --resume
atm-agent-mcp serve --fast
atm-agent-mcp serve --readonly

# Show resolved config and session context
atm-agent-mcp config

# List all sessions in registry
atm-agent-mcp sessions
atm-agent-mcp sessions --repo myapp       # filter by repo
atm-agent-mcp sessions --identity codex-architect

# Prune stale sessions and summaries
atm-agent-mcp sessions --prune

# Show summary for a session
atm-agent-mcp summary <agent-id>
```

### Claude MCP registration

```bash
claude mcp add codex -s user \
  -e PATH="/your/node/bin:/usr/local/bin:/usr/bin:/bin" \
  -- atm-agent-mcp serve
```

Or with identity baked in (for named agent slots):

```bash
claude mcp add codex-architect -s user \
  -e PATH="..." \
  -- atm-agent-mcp serve --identity codex-architect --role architect
```

---

## Future Considerations

- **Cross-machine threads** — once `atm-daemon` bridge is available, thread registry could be shared across machines
- **Approval via mail** — `approval_policy = "on-request"` sends approval request via `atm_send`, human replies, proxy unblocks Codex
- **Mail-to-thread routing config** — explicit rules in `.atm.toml` mapping senders or subjects to thread identities

---

## Files

| Path | Purpose |
|---|---|
| `crates/atm-agent-mcp/src/main.rs` | CLI entry point, argument parsing |
| `crates/atm-agent-mcp/src/proxy.rs` | stdio MCP proxy loop |
| `crates/atm-agent-mcp/src/config.rs` | Plugin config resolution via `atm-core` |
| `crates/atm-agent-mcp/src/identity.rs` | Session identity binding and `agent_id` mapping |
| `crates/atm-agent-mcp/src/context.rs` | Per-turn context refresh (repo, branch, cwd) |
| `crates/atm-agent-mcp/src/prompt.rs` | Bundled prompt loading and injection |
| `crates/atm-agent-mcp/src/registry.rs` | Session registry with atomic IO via `atm-core` |
| `crates/atm-agent-mcp/src/mail.rs` | Idle detection, inbox polling, mail routing |
| `crates/atm-agent-mcp/src/atm_tools.rs` | `atm_send` / `atm_read` / `atm_broadcast` MCP tools |
| `crates/atm-agent-mcp/src/summary.rs` | Shutdown summary request and file write/read |
| `crates/atm-agent-mcp/prompts/base.md` | Bundled Codex base prompt |
| `crates/atm-agent-mcp/prompts/multi-agent.md` | Bundled `experimental_prompt.md` |
