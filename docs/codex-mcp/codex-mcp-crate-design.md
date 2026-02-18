# `codex-mcp` Crate Design

A new crate in the `agent-team-mail` workspace: `crates/codex-mcp`.

A thin MCP proxy that wraps `codex mcp-server`, automatically injecting identity, team, repo context and system prompts from `.atm.toml`, managing named agent sessions, and delivering incoming `atm` mail as new Codex turns.

---

## Workspace Integration

```
agent-team-mail/
├── crates/
│   ├── atm-core/       # existing — config, IO, schema
│   ├── atm/            # existing — CLI
│   ├── atm-daemon/     # existing — daemon
│   └── codex-mcp/      # NEW — MCP proxy binary
```

Binary name: `codex-mcp`

Dependencies: `atm-core`, `tokio`, `serde_json`, `rmcp` (or raw stdio MCP), `signal-hook`

---

## Session Identity Model

Each running `codex-mcp` instance is a **named agent** with a unique identity within a team. Multiple instances can run simultaneously against the same repo, each with a distinct name.

```
Team: my-team
├── codex-architect   (codex-mcp serve --identity codex-architect)
├── codex-worker-1    (codex-mcp serve --identity codex-worker-1)
└── codex-worker-2    (codex-mcp serve --identity codex-worker-2)
```

Identity is resolved in priority order:
1. `--identity` CLI flag
2. `CODEX_MCP_IDENTITY` env var
3. `identity` in `[plugins.codex-mcp]` in `.atm.toml`
4. `identity` in `[core]` in `.atm.toml`
5. Default: `"codex"`

If the resolved identity is already active in the team registry, `codex-mcp` appends a suffix (`codex-2`, `codex-3`) to avoid collision, and logs the resolved name.

---

## Session Context

On startup, the proxy collects and stores session context that is injected into every Codex turn and persisted with the thread registry:

| Field | Source | Description |
|---|---|---|
| `identity` | config resolution | Agent's name on the team |
| `team` | `[core].default_team` | Team name |
| `repo_root` | git rev-parse --show-toplevel | Absolute path to git root |
| `repo_name` | git remote name or directory name | Human-readable repo identifier |
| `cwd` | process working directory | Launch directory (may differ from repo root) |
| `branch` | git rev-parse --abbrev-ref HEAD | Current branch at launch time |

If not in a git repo, `repo_root` and `repo_name` fall back to `cwd` and directory name respectively.

This context is injected into `developer-instructions` on every `codex` call, and stored in the thread registry entry so it can be displayed in `codex-mcp threads` and used when resuming.

---

## Configuration

Codex-specific config lives in `.atm.toml` under `[plugins.codex-mcp]`:

```toml
[core]
default_team = "my-team"
identity = "codex"              # default identity if --identity not passed

[plugins.codex-mcp]
# Path to codex binary (default: resolved from PATH)
codex_bin = "codex"

# Agent identity — overrides [core].identity for codex-mcp sessions
identity = "codex-architect"

# Model override (default: codex CLI default)
model = "o3"

# Reasoning effort (default: none)
reasoning_effort = "high"

# Sandbox mode (default: "workspace-write")
sandbox = "workspace-write"

# Approval policy (default: "on-failure")
approval_policy = "on-failure"

# Prompt files to inject (default: bundled gpt-5.2-codex + experimental)
base_prompt_file = ""          # empty = use bundled
extra_instructions_file = ""   # empty = use bundled experimental_prompt.md

# Incoming mail polling interval when idle, in milliseconds (default: 5000)
mail_poll_interval_ms = 5000

# How long to wait with no new mail before stopping idle polling (default: 300000 = 5min)
idle_timeout_ms = 300000

# Persist active thread IDs to disk for resume across restarts (default: true)
persist_threads = true

# Named role presets (see Roles section below)
[plugins.codex-mcp.roles.architect]
model = "o3"
reasoning_effort = "high"
sandbox = "read-only"

[plugins.codex-mcp.roles.worker]
model = "gpt-5.2-codex"
sandbox = "workspace-write"
approval_policy = "never"
```

Config resolution follows the same priority chain as `atm`:
1. CLI flags (`--identity`, `--role`, `--model`, `--sandbox`, `--approval-policy`)
2. Environment variables (`CODEX_MCP_IDENTITY`, `CODEX_MCP_MODEL`, `CODEX_MCP_SANDBOX`, etc.)
3. `[plugins.codex-mcp]` in repo-local `.atm.toml`
4. `[plugins.codex-mcp]` in `~/.config/atm/config.toml`
5. Defaults

---

## Architecture

```
claude (MCP client)
    │  stdio
    ▼
codex-mcp (proxy)
    │
    ├── on startup:
    │     resolve_config()          ← atm-core: reads .atm.toml, env, defaults
    │     resolve_identity()        ← identity + collision suffix if needed
    │     collect_session_context() ← repo_root, repo_name, branch, cwd
    │     load_prompts()            ← bundled gpt-5.2-codex + experimental
    │     load_thread_registry()    ← restore persisted thread IDs from disk
    │     register_with_team()      ← write agent entry to ~/.claude/teams/<team>/
    │     spawn codex mcp-server    ← child process, stdio piped
    │
    ├── MCP request intercept:
    │     tools/list      → pass through + add atm_send/atm_read/atm_broadcast
    │     codex           → inject developer-instructions, set cwd, then forward
    │     codex-reply     → forward, track threadId→identity mapping
    │     atm_send        → handled locally via atm-core (no shell needed)
    │     atm_read        → handled locally via atm-core
    │     atm_broadcast   → handled locally via atm-core
    │     *               → pass through
    │
    ├── on codex/codex-reply response:
    │     extract threadId          ← add to registry with identity + session context
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

Active threads are tracked in memory and persisted to `~/.config/atm/codex-sessions/registry.json`:

```json
{
  "version": 1,
  "sessions": [
    {
      "thread_id": "abc123",
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

Multiple sessions from different `codex-mcp` instances (different identities) all write to the same registry file using `atm-core` atomic IO. Each instance only manages its own sessions at shutdown, but can read the full registry for display.

**Shutdown behavior:**
- Graceful (SIGTERM): request summary from each own thread, write summary files, deregister, exit
- Forced (SIGKILL / parent disconnect): registry persists as-is for next session to resume from

---

## Prompt Injection

On every `codex` tool call, `codex-mcp` sets `cwd` to `repo_root` and injects into `developer-instructions`:

```
<session-context>
Identity:  {identity}
Team:      {team}
Repo:      {repo_name} ({repo_root})
Branch:    {branch}
</session-context>

<team-communication>
You can communicate with your team using the atm MCP tools available in this session:
  atm_send(to: "<agent>@{team}", message: "...")   # send to a team member
  atm_read()                                        # read your inbox
  atm_broadcast(message: "...")                     # send to all team members
Use these instead of shell `atm` commands — they are faster and auditable.
To discover teammates: atm_read() on startup, or check your team in context.
</team-communication>

<multi-agent>
[contents of experimental_prompt.md]
</multi-agent>
```

The full `base-instructions` is the bundled `gpt-5.2-codex_prompt.md` + the above, unless the caller has already provided `base-instructions` (caller-supplied base is respected, only `developer-instructions` is injected).

---

## ATM as MCP Tools

The proxy exposes `atm_send`, `atm_read`, and `atm_broadcast` as first-class MCP tools, implemented directly via `atm-core` — no shell execution required:

### `atm_send`
```json
{
  "to": "human@my-team",
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
2. If unread messages exist, format a digest and inject as `codex-reply` on the active thread:
   ```
   [Incoming mail]
   From: human@my-team — "Can you add rate limiting to the auth endpoint?"
   From: ci-agent@my-team — "Tests failing on feature/auth-refresh: timeout in test_token_expiry"
   ```
3. Mark messages as read
4. If no active thread — start a new `codex` session with the digest as the initial prompt
5. Resume polling after response completes

**Mail routing with multiple threads:**
- Mail addressed to this identity is delivered to the thread whose `tag` or `branch` matches content heuristics, or most-recently-active if ambiguous
- Mail with explicit `[thread:<id>]` prefix in subject routes directly to that thread (convention for agent-to-agent)

---

## Session Summary and Resume

On graceful shutdown, `codex-mcp` requests a compacted summary from each active thread:

```
Session ending. Write a concise summary of:
- What you were working on
- Current state — what is done, what is not
- Any open questions or blockers
- Next steps if resumed
```

Written to: `~/.config/atm/codex-sessions/<thread-id>/summary.md`

### Resuming with Context

```bash
# Resume most recent session for this identity
codex-mcp serve --resume-compacted

# Resume a specific thread by ID
codex-mcp serve --resume-compacted <thread-id>

# Resume by identity name (picks most recent session for that identity)
codex-mcp serve --resume-compacted codex-architect
```

The summary is prepended to `developer-instructions` on the first turn of the new session:

```
[Previous session — {identity} on {repo_name}/{branch}]
<contents of summary.md>
[End of previous session]
```

Summary files are kept until `codex-mcp threads --prune` or overwritten at next shutdown.

---

## CLI Interface

```bash
# Start the MCP server
codex-mcp serve
codex-mcp serve --identity codex-worker-1
codex-mcp serve --role worker
codex-mcp serve --resume-compacted

# Show resolved config and session context
codex-mcp config

# List all sessions in registry
codex-mcp threads
codex-mcp threads --repo myapp       # filter by repo
codex-mcp threads --identity codex-architect

# Prune stale sessions and summaries
codex-mcp threads --prune

# Show summary for a session
codex-mcp summary <thread-id>
```

### Claude MCP registration

```bash
claude mcp add codex -s user \
  -e PATH="/your/node/bin:/usr/local/bin:/usr/bin:/bin" \
  -- codex-mcp serve
```

Or with identity baked in (for named agent slots):

```bash
claude mcp add codex-architect -s user \
  -e PATH="..." \
  -- codex-mcp serve --identity codex-architect --role architect
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
| `crates/codex-mcp/src/main.rs` | CLI entry point, argument parsing |
| `crates/codex-mcp/src/proxy.rs` | stdio MCP proxy loop |
| `crates/codex-mcp/src/config.rs` | Plugin config resolution via `atm-core` |
| `crates/codex-mcp/src/identity.rs` | Identity resolution and collision handling |
| `crates/codex-mcp/src/context.rs` | Session context collection (repo, branch, cwd) |
| `crates/codex-mcp/src/prompt.rs` | Bundled prompt loading and injection |
| `crates/codex-mcp/src/registry.rs` | Thread registry with atomic IO via `atm-core` |
| `crates/codex-mcp/src/mail.rs` | Idle detection, inbox polling, mail routing |
| `crates/codex-mcp/src/atm_tools.rs` | `atm_send` / `atm_read` / `atm_broadcast` MCP tools |
| `crates/codex-mcp/src/summary.rs` | Shutdown summary request and file write/read |
| `crates/codex-mcp/prompts/base.md` | Bundled `gpt-5.2-codex_prompt.md` |
| `crates/codex-mcp/prompts/multi-agent.md` | Bundled `experimental_prompt.md` |
