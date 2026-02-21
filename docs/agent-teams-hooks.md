# Agent-Teams Hooks

This document describes all Claude Code hooks used in the `agent-team-mail` project, what they do, and why they exist.

Claude Code hooks are shell/Python scripts that fire at specific lifecycle points. They are configured in `.claude/settings.json` (project-level) or `~/.claude/settings.json` (global). Hook stdout is injected into Claude's context window; exit code `2` blocks the triggering action.

---

## Overview

| Hook | Scope | Trigger | Script | Purpose |
|------|-------|---------|--------|---------|
| `SessionStart` | Global | Session start, compact, resume | `~/.claude/scripts/session-start.py` | Announce session ID + ATM context; emit lifecycle event |
| `SessionEnd` | Global | Session exits | `~/.claude/scripts/session-end.py` | Emit lifecycle event to mark session dead |
| `PreToolUse(Task)` | Project | Every `Task` tool call | `.claude/scripts/gate-agent-spawns.py` | Enforce safe agent spawning rules |
| `TeammateIdle` | Project | Teammate goes idle | `.claude/scripts/teammate-idle-relay.py` | Relay idle lifecycle event to daemon |

---

## 1. Global SessionStart Hook

**Config**: `~/.claude/settings.json`
**Script**: `~/.claude/scripts/session-start.py`
**Fires**: On every interactive session startup, after `/compact`, and on `--continue` resume
**Scope**: All Claude Code sessions on this machine (global)

### What It Does

Reads the `SessionStart` payload from stdin and prints to stdout (injected into context):

1. **Always**: prints `SESSION_ID=<uuid> (starting fresh|returning from compact)`
2. **If `.atm.toml` present in cwd**: prints `ATM team: <default_team>`
3. **If `.atm.toml` has `welcome-message`**: prints the message text

Example output (injected at session start):
```
SESSION_ID=23551503-3d66-475c-acf2-dfa34f9d68b5 (starting fresh)
ATM team: atm-dev
Welcome: Read docs/project-plan.md before starting
```

### Why It Exists

**Problem**: `atm teams resume` needs the current session ID to update `leadSessionId` in team config. `CLAUDE_SESSION_ID` is set in Claude Code's process environment but is not exported to bash subshells — so the Rust binary called via Bash tool reads an empty or stale value.

**Solution**: The global hook fires before any tool calls and prints the session ID directly into Claude's context window. Claude can then pass it explicitly via `atm teams resume atm-dev --session-id <id>` (or set `CLAUDE_SESSION_ID` in-process before invoking ATM).

**Key facts about sessions**:
- Session ID is **stable across compaction** — `/compact` does NOT change the session ID
- Only a fresh `claude` invocation (new process) creates a new session ID
- `source: "compact"` in the payload distinguishes compact-resume from a fresh start
- This hook fires for **interactive sessions only** — Task-tool-spawned teammates do NOT trigger it

### `.atm.toml` `welcome-message` Field

Optional field in `[core]` section. If set, printed by the hook on every session start:

```toml
[core]
default_team = "atm-dev"
identity = "team-lead"
welcome-message = "Read docs/project-plan.md before starting"
```

---

## 2. Global SessionEnd Hook

**Config**: `~/.claude/settings.json`
**Script**: `~/.claude/scripts/session-end.py`
**Fires**: When a Claude Code session exits for any reason
**Scope**: All Claude Code sessions on this machine (global)

### What It Does

Reads the `SessionEnd` payload from stdin and notifies the ATM daemon that the session is ending:

1. Sends a `hook_event/session_end` message to the daemon Unix socket (if daemon is running)
2. Daemon marks the session as `Dead` in its session registry — enabling reliable liveness detection without PID polling

Example payload received:
```json
{
  "session_id": "23551503-3d66-475c-acf2-dfa34f9d68b5",
  "hook_event_name": "SessionEnd",
  "reason": "other",
  "transcript_path": "/Users/.../.claude/projects/.../transcript.jsonl",
  "cwd": "/Users/randlee/Documents/github/agent-team-mail"
}
```

The `reason` field can be `"clear"`, `"logout"`, `"prompt_input_exit"`, `"bypass_permissions_disabled"`, or `"other"`.

### Why It Exists

Without `SessionEnd`, the daemon must detect dead sessions by polling PIDs — which has a window where a crashed session looks alive. With `SessionEnd`, the daemon gets a clean notification and can immediately mark the session dead.

This enables:
- **`atm teams resume`** daemon guard: if previous team-lead's session is `Dead`, resume proceeds without `--force`
- **`atm teams cleanup`**: skip PID polling for recently-exited sessions already marked `Dead`
- **TUI live-state gate**: agent state transitions to `Closed` on session exit, disabling control input immediately

**`.atm.toml` guard**: Both `session-end.py` and `session-start.py` check for `.atm.toml` in `cwd` before contacting the daemon. If `.atm.toml` is absent, the daemon socket call is skipped entirely. This ensures the daemon only receives hook events from ATM project sessions — not from unrelated Claude Code sessions on the same machine.

**Fail-open**: The hook always exits `0`. If the daemon isn't running or `.atm.toml` is absent, the socket call is silently skipped.

**Note**: `SessionEnd` cannot block session termination — it is for cleanup/notification only.

---

## 3. Agent Spawn Gate (`PreToolUse`)

**Config**: `.claude/settings.json` (project-level, committed to repo)
**Script**: `.claude/scripts/gate-agent-spawns.py`
**Fires**: Before every `Task` tool call
**Scope**: This project only (all interactive sessions in this directory)

### What It Does

Enforces three rules before allowing a `Task` tool call to proceed. Exit code `2` blocks the call; `0` allows it.

#### Rule 1: Orchestrators Must Be Named Teammates

If `subagent_type` is in the `ORCHESTRATORS` set (currently: `scrum-master`) and no `name` parameter is provided, the call is **blocked**.

```
BLOCKED: 'scrum-master' is an orchestrator and must be a named teammate.

Correct:
  Task(subagent_type="scrum-master", name="sm-sprint-X", team_name="<team>", ...)

Wrong:
  Task(subagent_type="scrum-master", run_in_background=true)  # no name = dies at context limit
```

**Why**: Scrum-masters coordinate long-running sprints. Background agents (no `name`) cannot compact — they die when the context window fills. Named teammates run as full tmux processes and can compact to survive multi-hour sprints. The `name` parameter is the switch:
- **With `name`** → full tmux teammate (own pane, can compact, survives context limit)
- **Without `name`** → sidechain background agent (no pane, dies at context limit)

#### Rule 2: Only Team Lead Can Use `team_name`

If `team_name` is provided and the caller's `session_id` does **not** match `leadSessionId` in the team's `config.json`, the call is **blocked**.

```
BLOCKED: Only the team lead can spawn agents with team_name.

You are a teammate. Use background agents:
  Task(subagent_type="...", run_in_background=true, prompt="...")  # no team_name

NOT allowed from teammates:
  Task(..., team_name="atm-dev", ...)  # creates named teammate = pane exhaustion
```

**Why**: Each named teammate with `team_name` creates a new tmux pane. Without this gate, scrum-master orchestrators can accidentally spawn their own named sub-teammates, creating a pane explosion (3 scrum-masters × 2 sub-agents each = 9 panes). The correct pattern for scrum-masters is to spawn dev and QA as **background agents** (no `name`, no `team_name`).

The gate reads `leadSessionId` from `~/.claude/teams/<team>/config.json` and compares it to `session_id` in the hook payload. Only the session whose ID matches `leadSessionId` can pass a `team_name`.

**Fail-open behavior**: If no team config exists (new team) or `leadSessionId` is absent, the check is skipped and the call is allowed. This prevents the gate from blocking legitimate first-time setup.

#### Rule 3: `team_name` Must Match `.atm.toml`

If `team_name` is provided and `.atm.toml` exists in the project root with a `[core].default_team` value, the `team_name` must match.

```
BLOCKED: team_name must match .atm.toml core.default_team.

Required team_name: "atm-dev"
Got team_name:      "wrong-team"
```

**Why**: Prevents accidentally targeting the wrong team, which would route ATM messages to the wrong inbox and lose communications with agents like arch-ctm.

### Debug Log

Every hook call (pass or block) is appended to `${TMPDIR}/gate-agent-spawns-debug.jsonl` (platform temp dir).
The hook also writes `${TMPDIR}/atm-session-id` as an audit/debug breadcrumb.
This breadcrumb is **not** the production session resolution path for `atm teams resume`;
resume should use explicit `--session-id` and/or `CLAUDE_SESSION_ID`.

### What Is NOT Blocked

- Spawning any non-orchestrator agent type (e.g., `rust-developer`, `rust-qa-agent`, `general-purpose`) — with or without `name`
- Background agents spawned without `team_name` by teammates (the dev/QA pattern used by scrum-masters)
- Any call where `team_name` matches the configured default and `session_id` matches `leadSessionId`

---

## 4. TeammateIdle Relay

**Config**: `.claude/settings.json` (project-level)
**Script**: `.claude/scripts/teammate-idle-relay.py`
**Fires**: When any teammate goes idle
**Scope**: This project only

### What It Does

Reads the `TeammateIdle` payload from stdin, enriches it with ATM identity/team context, and appends one JSON line to the daemon's hook event log:

```
${ATM_HOME:-$HOME}/.claude/daemon/hooks/events.jsonl
```

The event has the shape:
```json
{
  "type": "teammate-idle",
  "agent": "<agent name>",
  "team": "<team name>",
  "session_id": "<uuid>",
  "received_at": "2026-02-20T12:00:00Z",
  "payload": { ... original hook payload ... }
}
```

Team name is resolved in priority order: payload `team_name` → env `ATM_TEAM` → `.atm.toml` `default_team`.
Agent name is resolved from: payload `name` → payload `agent` → env `ATM_IDENTITY`.

The relay also sends the same lifecycle signal to the daemon socket (`command: "hook-event"`)
for low-latency state updates, while keeping `events.jsonl` as a durable audit trail.

### Why It Exists

The ATM daemon tracks agent activity state (active, idle, killed, etc.) for features like:
- Live-state gating in the TUI (enable/disable control input based on agent state)
- `atm teams cleanup` liveness checks (skip alive agents, remove dead ones)
- Session registry for `atm teams resume` daemon-guarded lead claim (Phase E.2)

The `TeammateIdle` hook is the signal that a teammate has finished a turn and is waiting. By relaying this event to the daemon's event log, the daemon can update its internal activity model without polling or requiring agents to explicitly call `atm`.

**Fail-open**: The script always exits `0` regardless of errors. A relay failure should never block a teammate from continuing work.

---

## Extensible Lifecycle Sources

Daemon lifecycle handling should remain on one command path (`hook-event`) with a source-kind discriminator
so Claude hooks, MCP, and future adapters share one state machine.

Recommended `source` kinds:
- `claude_hook` — this document's hooks
- `atm_mcp` — lifecycle events emitted by `atm-agent-mcp`
- `agent_hook` — future provider hooks/adapters (for example Codex/Gemini if/when exposed)
- `unknown` — default fallback

When non-Claude adapters gain lifecycle callbacks, they should emit `session_start`, `teammate_idle`,
and `session_end` using the same envelope and daemon command path.

---

## Hook Configuration Files

### Project-level: `.claude/settings.json`

Committed to the repository. Applies to all interactive Claude Code sessions opened in this project directory.

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Task",
        "hooks": [
          {
            "type": "command",
            "command": "python3 .claude/scripts/gate-agent-spawns.py"
          }
        ]
      }
    ],
    "TeammateIdle": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 .claude/scripts/teammate-idle-relay.py"
          }
        ]
      }
    ]
  }
}
```

### Global: `~/.claude/settings.json`

Personal machine settings. Applies to all Claude Code sessions regardless of project.

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 ~/.claude/scripts/session-start.py"
          }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 ~/.claude/scripts/session-end.py"
          }
        ]
      }
    ]
  }
}
```

---

## Hook Payload Reference

Each hook receives a JSON payload on stdin. Key fields:

| Field | Present In | Description |
|-------|-----------|-------------|
| `session_id` | All hooks | UUID of the calling Claude Code session |
| `source` | `SessionStart` | `"init"` (fresh start), `"compact"` (post-compaction), `"resume"` (--continue) |
| `reason` | `SessionEnd` | `"clear"`, `"logout"`, `"prompt_input_exit"`, `"bypass_permissions_disabled"`, `"other"` |
| `transcript_path` | `SessionEnd` | Path to the session transcript JSONL file |
| `tool_name` | `PreToolUse` | Name of the tool being called (e.g., `"Task"`) |
| `tool_input` | `PreToolUse` | The tool's input parameters as a JSON object |
| `tool_input.subagent_type` | `PreToolUse(Task)` | Agent type being spawned |
| `tool_input.name` | `PreToolUse(Task)` | Teammate name (if present, spawns named tmux teammate) |
| `tool_input.team_name` | `PreToolUse(Task)` | Team to join (if present, adds to team) |
| `name` | `TeammateIdle` | Name of the idle teammate |
| `team_name` | `TeammateIdle` | Team the teammate belongs to |

---

## Hook Implementation Standards

All hook scripts follow these conventions:

- **Python only** — no bash scripts. Python is cross-platform (macOS, Linux, Windows) and testable with standard `unittest` / `pytest`.
- **Fail-open always** — every script exits `0` regardless of errors. Hooks must never block Claude Code operation.
- **`.atm.toml` guard** — scripts that contact the daemon MUST check for `.atm.toml` in `cwd` before any socket call. If absent, skip silently. This scopes daemon communication to ATM project sessions only.
- **Unit tests** — each hook script has a corresponding test file in `.claude/scripts/tests/`. Tests cover: correct message shape, `.atm.toml` guard behavior, socket-error fail-open, daemon-not-running fail-open.
- **No side effects on missing deps** — if `jq`, a socket, or a file path is unavailable, the script degrades gracefully with no output.

---

## Known Limitations

- **Project-level hooks only fire for interactive sessions.** Task-tool-spawned teammates (background agents or named teammates) do NOT trigger `PreToolUse` or `TeammateIdle` for their own Tool calls — only the parent session's hook fires when spawning them.
- **`SessionStart` is global, not project-scoped.** All sessions on the machine get the hook output, even in unrelated projects. The hook gracefully does nothing if `.atm.toml` is absent.
- **`leadSessionId` must be current** for Rule 2 to work correctly. If `atm teams resume` has not been run after a session restart, the gate may incorrectly block the team lead. See `atm teams resume` documentation and issue [#141](https://github.com/randlee/agent-team-mail/issues/141).
- **Agent-teams is pre-release** as of Claude Code v2.1.39. Hook behavior may change in future versions.
