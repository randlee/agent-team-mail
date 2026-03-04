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
| `PreToolUse(Bash)` | Project | Every `Bash` tool call with `atm` command | `.claude/scripts/atm-identity-write.py` | Write hook file for PID-based identity correlation (Phase N.2) |
| `PostToolUse(Bash)` | Project | After every `Bash` tool call | `.claude/scripts/atm-identity-cleanup.py` | Delete hook file after tool completes (Phase N.2) |
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
- **Daemon teardown convergence**: once session death is confirmed, daemon can reconcile
  coupled cleanup (remove roster entry + delete mailbox) without waiting for long PID sweep windows

**`.atm.toml` guard**: Both `session-end.py` and `session-start.py` check for `.atm.toml` in `cwd` before contacting the daemon. If `.atm.toml` is absent, the daemon socket call is skipped entirely. This ensures the daemon only receives hook events from ATM project sessions — not from unrelated Claude Code sessions on the same machine.

**Fail-open**: The hook always exits `0`. If the daemon isn't running or `.atm.toml` is absent, the socket call is silently skipped.

**Note**: `SessionEnd` cannot block session termination — it is for cleanup/notification only.
`shutdown_request` remains the active termination mechanism; mailbox deletion is not a termination signal.

---

## 3. Agent Spawn Gate (`PreToolUse`)

**Config**: `.claude/settings.json` (project-level, committed to repo)
**Script**: `.claude/scripts/gate-agent-spawns.py`
**Fires**: Before every `Task` tool call
**Scope**: This project only (all interactive sessions in this directory)

### What It Does

Enforces three rules before allowing a `Task` tool call to proceed. Exit code `2` blocks the call; `0` allows it.

#### Rule 1: Orchestrators Must Be Named Teammates

If the target prompt file (`.claude/agents/<subagent_type>.md`) declares frontmatter `metadata.spawn_policy: named_teammate_required` and no `name` parameter is provided, the call is **blocked**.

```
BLOCKED: 'scrum-master' requires named teammate spawn policy.

Correct:
  Task(subagent_type="scrum-master", name="sm-sprint-X", team_name="<team>", ...)

Wrong:
  Task(subagent_type="scrum-master", run_in_background=true)  # no name = dies at context limit
```

**Why**: Some roles coordinate long-running workflows. Background agents (no `name`) cannot compact — they die when the context window fills. Named teammates run as full tmux processes and can compact to survive multi-hour work. The `name` parameter is the switch:
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

**Why**: Each named teammate with `team_name` creates a new tmux pane. Without this gate, orchestrators can accidentally spawn their own named sub-teammates, creating a pane explosion (for example: 3 scrum-masters × 2 sub-agents each = 9 panes). The correct pattern for orchestrators is to spawn implementation/review helpers as **background agents** (no `name`, no `team_name`).

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
Each log entry includes `process_id` (the OS PID of the hook script invocation) for diagnostics.
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
  "process_id": 12345,
  "received_at": "2026-02-20T12:00:00Z",
  "payload": { ... original hook payload ... }
}
```

Team name is resolved in priority order: payload `team_name` → env `ATM_TEAM` → `.atm.toml` `default_team`.
Agent name is resolved from: payload `teammate_name` → payload `name` → payload `agent` → env `ATM_IDENTITY`.

> **Note**: Claude Code sends the teammate's name as `teammate_name` in the `TeammateIdle` hook payload (not `name`). The `name` and `agent` fallbacks exist for manual testing and forward compatibility.

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

Daemon lifecycle handling uses one command path (`hook-event`) with a `source.kind` discriminator
so Claude hooks, MCP proxies, and future adapters share one state machine.

### `source.kind` Values

| `kind`        | Emitted by                              | `session_start` / `session_end` restriction |
|---------------|-----------------------------------------|---------------------------------------------|
| `claude_hook` | Claude Code hook scripts (this document)| Team-lead only (strictest)                  |
| `atm_mcp`     | `atm-agent-mcp` proxy                  | Any team member                             |
| `agent_hook`  | Future non-Claude agent relay scripts   | Any team member                             |
| `unknown`     | Absent or unrecognised field            | Treated as `claude_hook` (fail-closed)      |

The `source` field is **optional** for backward compatibility. Payloads that omit it parse
correctly and default to `unknown` (strictest validation).

---

## Wiring a New Lifecycle Adapter

When a new agent runtime gains lifecycle callbacks (e.g., a Codex Gemini fork or an internal
CI agent), connect it to the ATM daemon by implementing one of two patterns.

### Pattern A — Hook Relay Script (external, file-based)

Use this pattern when the agent runtime fires shell-level hooks (similar to Claude Code's
`TeammateIdle` hook). The relay script appends one JSON line to `events.jsonl` **and** sends
a direct socket call to the daemon.

Reference implementation: `.claude/scripts/teammate-idle-relay.py`

Steps:
1. Write a script that receives the hook payload on stdin.
2. Resolve `agent` name and `team` from the payload or environment.
3. Build the `hook-event` payload:

```json
{
  "command": "hook-event",
  "payload": {
    "event": "session_start",
    "agent": "<agent-name>",
    "team": "<team-name>",
    "session_id": "<runtime-session-id>",
    "source": {"kind": "agent_hook"}
  }
}
```

4. Send it to `${ATM_HOME}/.claude/daemon/atm-daemon.sock` using the newline-delimited
   protocol (one JSON line per request, one JSON line response).
5. Exit `0` regardless of errors — lifecycle relay must never block agent execution.

Use `source.kind = "agent_hook"` so the daemon permits non-lead agents to emit
`session_start` and `session_end` events.

### Pattern B — In-Process Emission (MCP proxy or library)

Use this pattern when you control the proxy or library that wraps the agent runtime.
Emit lifecycle events directly from Rust (or another async language) without a relay script.

Reference implementation: `crates/atm-agent-mcp/src/lifecycle_emit.rs`

Key design principles:
- Use `tokio::spawn` to fire-and-forget the emission; do **not** `await` it inline.
- Wrap every emission in a `warn!` log on error — never propagate errors to the caller.
- Gate the Unix socket call with `#[cfg(unix)]` — the function must compile and no-op on Windows.
- Set `source.kind = "atm_mcp"` (or `"agent_hook"`) in the payload.

Minimal example:

```rust
use atm_agent_mcp::lifecycle_emit::{emit_lifecycle_event, EventKind};

// After session registration:
tokio::spawn(async move {
    emit_lifecycle_event(
        EventKind::SessionStart,
        &identity,
        &team,
        &session_id,
        Some(process_id),
    ).await;
});

// After a turn completes (thread → Idle):
tokio::spawn(async move {
    emit_lifecycle_event(EventKind::TeammateIdle, &identity, &team, &agent_id, None).await;
});

// On session close:
tokio::spawn(async move {
    emit_lifecycle_event(EventKind::SessionEnd, &identity, &team, &agent_id, None).await;
});
```

### Validation Policy Summary

The daemon enforces different rules depending on `source.kind`:

- **`claude_hook` / `unknown`** (strictest): only the team-lead may emit `session_start` or
  `session_end`. This protects the team-lead's Claude Code session record from accidental
  overwrite by untrusted sources.
- **`atm_mcp` / `agent_hook`** (relaxed): any team member registered in `config.json` may
  emit lifecycle events. This is necessary because MCP proxies and external adapters manage
  their own agent sessions, not the team-lead's session.
- **All sources**: the `agent` field must be a registered team member in the named team.
  Unknown agents are rejected regardless of source kind.

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
            "command": "python3 \"$CLAUDE_PROJECT_DIR/.claude/scripts/gate-agent-spawns.py\""
          }
        ]
      }
    ],
    "TeammateIdle": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 \"$CLAUDE_PROJECT_DIR/.claude/scripts/teammate-idle-relay.py\""
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

There are two payload layers:

1. Claude Code hook stdin payload (raw event from Claude)
2. ATM daemon socket payload (`command: "hook-event"`) emitted by ATM scripts/proxies

### 1) Claude Hook Stdin Payload

| Field | Present In | Description |
|-------|-----------|-------------|
| `session_id` | All hooks | UUID of the calling Claude Code session |
| `source` | `SessionStart` | Claude-native start mode: `"init"` (fresh), `"compact"` (post-compaction), `"resume"` (`--continue`) |
| `reason` | `SessionEnd` | `"clear"`, `"logout"`, `"prompt_input_exit"`, `"bypass_permissions_disabled"`, `"other"` |
| `transcript_path` | `SessionEnd` | Path to the session transcript JSONL file |
| `tool_name` | `PreToolUse` | Name of the tool being called (e.g., `"Task"`) |
| `tool_input` | `PreToolUse` | The tool's input parameters as a JSON object |
| `tool_input.subagent_type` | `PreToolUse(Task)` | Agent type being spawned |
| `tool_input.name` | `PreToolUse(Task)` | Teammate name (if present, spawns named tmux teammate) |
| `tool_input.team_name` | `PreToolUse(Task)` | Team to join (if present, adds to team) |
| `teammate_name` | `TeammateIdle` | Name of the idle teammate (Claude Code's actual field name) |
| `team_name` | `TeammateIdle` | Team the teammate belongs to |
| `transcript_path` | `PreToolUse`, `TeammateIdle` | Path to the session transcript JSONL file |
| `cwd` | `PreToolUse`, `TeammateIdle` | Working directory of the calling session |
| `permission_mode` | `PreToolUse`, `TeammateIdle` | Permission mode (e.g., `"bypassPermissions"`) |
| `hook_event_name` | All hooks | Hook type identifier (e.g., `"PreToolUse"`, `"TeammateIdle"`) |
| `tool_use_id` | `PreToolUse` | Unique ID for the tool invocation |

> **Important**: PreToolUse payloads do **not** include any agent/teammate identity field.
> The only hook that provides `teammate_name` is `TeammateIdle`.

### 2) ATM Daemon Socket Payload (`hook-event`)

| Field | Description |
|-------|-------------|
| `event` | `session_start` \| `teammate_idle` \| `session_end` |
| `session_id` | Session identifier used by daemon liveness/state tracking |
| `agent` | ATM member identity |
| `team` | ATM team name |
| `process_id` | OS process ID of the parent agent session (`os.getppid()` from hook) used for liveness correlation |
| `source.kind` | Lifecycle source discriminator (`claude_hook`, `atm_mcp`, `agent_hook`, `unknown`) |

---

## Hook Implementation Standards

All hook scripts follow these conventions:

- **Python only** — no bash scripts. Python is cross-platform (macOS, Linux, Windows) and testable with standard `unittest` / `pytest`.
- **Fail-open always** — every script exits `0` regardless of errors. Hooks must never block Claude Code operation.
- **`.atm.toml` guard** — scripts that contact the daemon MUST check for `.atm.toml` in `cwd` before any socket call. If absent, skip silently. This scopes daemon communication to ATM project sessions only.
- **Unit tests** — each hook script has a corresponding test file in `tests/hook-scripts/`. Tests cover: correct message shape, `.atm.toml` guard behavior, socket-error fail-open, daemon-not-running fail-open.
- **No side effects on missing deps** — if `jq`, a socket, or a file path is unavailable, the script degrades gracefully with no output.

---

## Known Limitations

- **PreToolUse hooks stop firing in the lead session after compaction.** Tested: 309 entries from prior sessions, 0 entries post-compaction in the same session. TeammateIdle hooks survive compaction. Workaround: start a fresh `claude` session.
- **PreToolUse hooks DO fire for tmux teammates.** Each teammate is a separate Claude Code process; project hooks in `.claude/settings.json` apply to their tool calls. Confirmed: Bash, Read, and SendMessage all trigger PreToolUse in the teammate's session.
- **Hook scripts resolve from the teammate's `cwd`** (the main repo), not from any worktree. Confirmed via `hook_source` tagging experiment.
- **PreToolUse payload does NOT include agent/teammate identity.** Only `session_id`, `tool_name`, `tool_input`, `tool_use_id`, `cwd`, `transcript_path`, `permission_mode`. No `teammate_name` or equivalent.
- **TeammateIdle is the only hook with `teammate_name`.** The field is `teammate_name` (not `name`). It fires after the agent goes idle, not before tool calls.
- **Each hook invocation gets a fresh PID** but `os.getppid()` from the hook is the stable agent process PID for the session. This can be used for cross-process identity correlation (see Phase N.2 design).
- **SessionStart may have a race condition for tmux teammates.** The tmux pane may not finish shell initialization before the claude command is sent via `send-keys`. Unverified from official docs but consistent with observed behavior (0 SessionStart entries for teammates).
- **`SessionStart` is global, not project-scoped.** All sessions on the machine get the hook output, even in unrelated projects. The hook gracefully does nothing if `.atm.toml` is absent.
- **`leadSessionId` must be current** for Rule 2 to work correctly. If `atm teams resume` has not been run after a session restart, the gate may incorrectly block the team lead. See `atm teams resume` documentation and issue [#141](https://github.com/randlee/agent-team-mail/issues/141).
- **Daemon session registry is currently keyed by agent name only.** Cross-team duplicate member names can collide in session tracking until the registry is migrated to a team-scoped key (`(team, name)`).
- **Agent-teams is pre-release** as of Claude Code v2.1.39. Hook behavior may change in future versions.

---

## PID-Based Identity Correlation (Phase N.2 Design)

Tested approach for resolving agent identity at tool-use time without `teammate_name` in PreToolUse. Verified on macOS, Windows native, and WSL.

### Process Tree Structure (confirmed)

```
Agent PID (stable per session, e.g., 11449)
├── PreToolUse hook (child, ppid = agent PID)  ← writes hook file
├── Bash tool → shell (child, ppid = agent PID)
│   └── atm send/read (grandchild, ppid = shell) ← reads hook file (PostToolUse deletes)
├── TeammateIdle hook (child, ppid = agent PID)
```

### Write: PreToolUse Hook (every tool call)

The PreToolUse hook fires before every tool call as a child process of the agent.

- **Only writes for `atm` commands** — parses command tokens and matches `atm` invocation patterns (including `cargo run ... atm` forms); skips all other tool calls (no orphan files). This is a Python script and can be hand-edited if custom aliases or wrappers are used.
- **File**: `<temp_dir>/atm-hook-<hook_pid>.json` where `hook_pid` = `os.getpid()` (changes every call)
- **Permissions**: `0600` (owner-only read/write) on Unix — prevents spoofing/tampering in shared temp directories. On Windows, ACL-based ownership validation is not portable; implementation should log a warning and skip ownership check rather than fail (fail-open).
- **Contents**: `{ pid: <agent_pid>, session_id, agent_name, created_at }`
  - `agent_pid` = `os.getppid()` — the hook's parent is the stable agent process
  - `session_id` — from the hook's stdin payload
  - `agent_name` — resolved from daemon registry (populated by `atm register`) or null if not yet registered
  - `created_at` — monotonic/epoch timestamp for staleness detection

A new file is created on every tool call because each hook invocation gets a fresh PID.

### Read: `atm` Command (spawned by Bash tool)

When `atm send`, `atm read`, etc. runs inside a Bash tool call:

1. `atm` gets its **parent PID** via `getppid()` — this is the shell spawned by the Bash tool
2. Opens `<temp_dir>/atm-hook-<parent_pid>.json` — **direct file lookup**, no scanning
3. **Validates before trusting**:
   - File owner matches current user on Unix (prevents cross-user spoofing); skipped on Windows with warning
   - `created_at` is within max age (e.g., 5 seconds) — rejects stale files from crashed processes or PID reuse
   - `session_id` matches expected session if known (guard against PID reuse misattribution)
4. Reads `session_id` + `agent_name` from the file
5. **Does not delete** — file is cleaned up by PostToolUse hook (supports multiple `atm` calls in one Bash invocation)
6. **Missing or unreadable file**: `atm` **rejects the call** and requires an explicit identity override by command contract:
   - `atm send` / `atm broadcast`: require `--from <name>`
   - `atm read` (own inbox mode): require `--as <name>`
   No silent fallback — a missing hook file indicates a broken hook setup and should not be masked. Same behavior for locked, permission-denied, or corrupt files.

This works because the PreToolUse hook and the Bash shell share the same PID — Claude Code runs both as the same child process of the agent. The hook runs first (and can block with exit code 2), then the shell executes in the same process. So `atm`'s parent PID is always the hook file's name.

**Multiple `atm` calls in one Bash invocation**: If a shell script runs `atm send foo && atm send bar`, deleting on first read would leave the second call without identity. Instead, `atm` **reads without deleting**. Cleanup is handled by a **PostToolUse hook** that deletes the hook file after the tool call completes. This ensures all `atm` invocations within a single Bash call share the same identity context, and the file is cleaned up reliably regardless of how many (or zero) `atm` calls ran. The `created_at` TTL provides additional staleness protection.

### `atm register` — Unified Session Registration (replaces `atm teams resume`)

One command handles both team-lead and teammate registration. Called once per session at startup (instructed via CLAUDE.md). Reads the hook file for `session_id` automatically.

Session ID source policy for `register`:
- Primary: hook file (`<temp_dir>/atm-hook-<pid>.json`)
- Bootstrap fallback (register only): `CLAUDE_SESSION_ID` with a warning if hook file is unavailable
- If neither source is available: reject and require explicit remediation (fix hooks or pass an explicit session identifier if supported)

#### Team-lead: `atm register <team>`

Team-lead calls first (identity resolved from `.atm.toml`). This subsumes the current `atm teams resume` functionality:

- Reads hook file → gets `session_id` automatically
- Claims team lead: updates `leadSessionId` in team config
- Auto-backup before state change
- Liveness check on old session (daemon query, `--force` to override)
- If another team-lead session appears active, do not auto-assume role:
  - warn and block by default
  - allow explicit takeover with `--force`
  - optional `--force --kill` may terminate prior lead PID only when PID is known and user explicitly requested it
- Marks non-lead members inactive
- Notifies all members via inbox
- Registers `{ agent_name: "team-lead", session_id, agent_pid }` with daemon

#### Teammates: `atm register <team> <name>`

Teammates call after team-lead has claimed. Passes their name explicitly:

- Reads hook file → gets `session_id` automatically
- Registers `{ agent_name, session_id, agent_pid }` with daemon
- Solves the `agent_name` timing gap (TeammateIdle hasn't fired yet on first call)
- Subsequent PreToolUse hook files can populate `agent_name` from the daemon registry

#### CLAUDE.md Instructions

```markdown
# First thing every session:
atm register <team>           # team-lead (name from .atm.toml identity)
atm register <team> <name>    # teammates (name passed explicitly)
```

#### Migration from `atm teams resume`

`atm teams resume` continues to work as an alias during the transition. The key improvement is that `register` reads `session_id` from the hook file (written by PreToolUse before the Bash tool runs), eliminating the biggest pain point: having to manually pass `--session-id` every session.

### Identity Resolution Order

For Claude Code agents (hook file expected):
```
1. Hook file (<temp_dir>/atm-hook-<pid>.json)        → required when identity is not explicitly provided
2. Explicit override (`--from` for send, `--as` read) → always allowed
3. If neither available                               → REJECT and ask user to identify agent explicitly
```

For non-Claude agents (Codex, Gemini, external — no hooks):
```
1. Explicit CLI identity override (`--from` / `--as`)   → always works
2. ATM_IDENTITY env var                                  → Codex/Gemini/external agents
3. .atm.toml [core].identity                             → fallback only when set to a concrete team member
4. No identity resolved                                  → REJECT; do not silently assign `human`
```

Explicit identity flags (`--from`, `--as`) work regardless of hook state.

### Register Ordering

The intended order is team-lead registers first (the team is created by the lead's Claude Code session via `TeamCreate`), but teammate-first registration is allowed with a warning.

**All members must register.** Registration validates that the provided `<name>` exists in the team's `config.json` members list. Unknown names are rejected — this prevents typos and impersonation. There is no implicit member creation through register; members must already exist in config.json (added via `TeamCreate` or `atm teams add-member`).

If a teammate calls `register <team> <name>` before team-lead has registered:
- **Allow it** — register the teammate with the provided name (update their entry in config.json)
- **Output a warning**: `WARNING: team-lead is not registered for this team. ATM messaging will work, but Claude Code team messaging (SendMessage) will not function until team-lead registers. If this is unexpected, ask the user: "Who am I on this team?"`
- The teammate can send/receive ATM messages (file-based), but Claude Code's built-in team messaging requires `leadSessionId` to be current

When team-lead registers later, everything starts working. The teammate doesn't need to re-register.

If registration fails (name not in config, team not found, etc.), the error message should strongly indicate: **ask the user who you are on this team** rather than guessing or proceeding without identity.

### Open Questions

- **`atm teams resume` deprecation timeline**: Keep as alias indefinitely, or sunset after one phase?

### Cross-Platform Notes

- `os.getpid()` and `os.getppid()` — cross-platform (Python 3.2+, Rust `std::process::id()`)
- Parent PID from Rust: `std::os::unix::process::parent_id()` on Unix; on Windows use `winapi` or `sysinfo` crate
- Hook file path: use `std::env::temp_dir()` (Rust) / `tempfile.gettempdir()` (Python) for cross-platform temp directory
- `atm_hook_lib.py` abstracts socket communication (Unix domain on macOS/Linux, TCP on Windows)
