# Agent Teams Mail Skill — Background & Findings

**Status**: Active / Evolving
**Date**: 2026-02-20 (updated from 2026-02-11)

---

## Background

This document captures experimental findings about the Claude Code Agent Teams messaging system that inform the design of `atm` as a mail-like overlay. These findings were gathered through hands-on testing of agent lifecycle, shutdown/respawn, and offline message queuing behaviors.

## Key Findings

### 1. Inbox Is Durable, Agents Are Ephemeral

The inbox system is file-based (`~/.claude/teams/{team}/inboxes/{name}.json`). Files persist across agent lifetimes. Agents are ephemeral processes that come and go. This is a natural fit for a mail-like abstraction — the inbox is the mailbox, agents are consumers.

### 2. Message Routing Is Name-Based

All routing uses agent `name`, not `agentId`. The `agentId` (`name@team`) is deterministic and exists only in `config.json` for internal bookkeeping. Inbox files, message `from`/`to` fields — all use names. This means any agent with the same name inherits the same inbox.

### 3. Offline Message Delivery Is Silent

`SendMessage` to a shut-down agent succeeds without error or warning. The message is written to the inbox file with `read: false`. The sender receives no indication that the recipient is offline. Messages queue indefinitely.

### 4. Queued Messages Are Visible But Not Reliably Acted On

`atm read` uses `--since-last-seen` cursor filtering by default, so only messages since the last read watermark are shown — not the full inbox history. This is correct behavior for normal use.

However, when an agent respawns with a **new session**, the last-seen watermark may not transfer, meaning the agent may see a burst of historical messages. The agent may not act on all of them:

- **Clean inbox** (few messages): Plain instructions are generally acted on
- **Noisy inbox** (many old messages): Plain instructions are treated as stale history and ignored
- **With call-to-action tag**: Instructions prefixed with `[PENDING ACTION]` or `[OFFLINE MESSAGE - Acknowledge and respond]` are reliably acted on regardless of inbox noise

The root cause is disambiguation — without a signal, the agent cannot distinguish "new task waiting for me" from "old task that was already handled by a previous instance."

**Mitigation**: Use `atm teams cleanup` after session resume to prune dead agents' inboxes before they accumulate. Keep inboxes lean — large inboxes waste context window.

### 5. Online/Offline Detection via `isActive`

The `isActive` field in `config.json` member entries provides a usable (if imperfect) signal:

| State | `isActive` | In members? | Meaning |
|-------|-----------|-------------|---------|
| Running | `true` | Yes | Agent process is alive (running or idle-waiting) |
| Shut down (retained) | `false` | Yes | Agent terminated, entry remains |
| Shut down (removed) | N/A | No | Agent terminated, entry cleaned up |
| Never existed | N/A | No | No inbox file either |

**Detection logic for `atm send`**:
```
if member not found OR member.isActive == false → offline
```

**Keeping `isActive` accurate**: The field is only reliable if actively maintained:
- **Codex agents**: daemon tracks liveness via `SessionStart` hook + PID polling → sets `isActive` accurately
- **Claude Code agents**: `isActive` is set by Claude Code infrastructure but can go stale on crash
- **On `atm teams resume`**: set `isActive=false` for all Claude members except team-lead — this resets stale state and ensures only the current team-lead is marked active at session start
- **On `atm teams cleanup <agent>`**: removes dead member from config entirely

**Caveat**: Shutdown behavior is inconsistent — some agents are removed from the members array on shutdown, others remain with `isActive: false`. Both cases are handled by the detection logic above. Crashes leave `isActive: true` until daemon detects via PID poll or `resume` resets it.

### 6. Shutdown Removes Agent from Members (Inconsistently)

Agent shutdown via `shutdown_request` → `shutdown_response(approve: true)` sometimes removes the agent from `config.json` members array entirely, and sometimes leaves the entry with `isActive: false`. Both patterns were observed in the same team during testing. The inconsistency may relate to timing, concurrent operations, or whether a respawn occurred before cleanup completed.

### 7. Session ID Mismatch Creates Orphan Teams

`leadSessionId` in `config.json` is set when the team is first created and must match the current Claude Code session ID (`CLAUDE_SESSION_ID`) for message delivery to work. On each new Claude Code session, a new `CLAUDE_SESSION_ID` is generated.

**What breaks**: If `leadSessionId` doesn't match, Claude Code creates a **new team with a random auto-generated name** instead of rejoining the existing team. Since `.atm.toml` hardcodes `default_team=atm-dev`, non-Claude teammates (arch-ctm) become unreachable.

**`CLAUDE_SESSION_ID` availability**:
- Set in Claude Code's process environment
- **NOT exported to bash subshells** — `echo $CLAUDE_SESSION_ID` returns empty
- **IS inherited by Rust binaries** spawned by Claude Code (e.g., `atm` reads it directly)
- This means `atm teams resume` can read it natively without bash workarounds

**Claude Code caches `leadSessionId` at startup** — patching `config.json` mid-session does not fix message delivery for the current session. The correct session ID must be in place before the session starts.

**Fix**: `atm teams resume <team>` — reads `CLAUDE_SESSION_ID` from process env, updates `leadSessionId`, notifies members, outputs the `TeamCreate` call needed to re-establish the team. Run before `TeamCreate` at every session start.

### 8. Name Auto-Increment from Stale Config Entries

When spawning a teammate with `name: "publisher"`, Claude Code checks the `members` array in `config.json`. If a member named `publisher` already exists (even if dead/stale), the new spawn becomes `publisher-2`, then `publisher-3`, etc.

**This breaks ATM identity**: The agent's prompt may reference `publisher` as its ATM identity, but it spawns as `publisher-3`. Its `ATM_IDENTITY` env var won't be set (unless passed explicitly), so `atm` CLI falls back to `.atm.toml` identity (`team-lead`) — causing the agent to read and send as team-lead instead of itself.

**Fix**: Run `atm teams cleanup atm-dev` after `resume` to remove stale members from `config.json` before spawning new teammates. This ensures `publisher` spawns as `publisher`, not `publisher-3`.

### 9. ATM Identity vs Claude Code Identity Are Independent

Agents have two separate identities that must be kept in sync:
- **Claude Code identity**: the `name` field in `config.json` (e.g., `publisher-3`) — used for `SendMessage` routing
- **ATM identity**: resolved from `ATM_IDENTITY` env var → `.atm.toml` → default `"human"` — used for `atm send/read`

If these diverge, the agent's `SendMessage` replies arrive correctly in the inbox, but `atm read` reads the wrong inbox (e.g., team-lead's instead of publisher's).

**Fix**: Always set `ATM_IDENTITY=<name>` in the agent's spawn environment. For tmux workers, `launch-worker.sh` does this automatically. For Claude Code teammates spawned via `Task`, pass the identity in the prompt and instruct the agent to use `atm send --from <name>`.

### 10. Inbox Is Delivery, Not a Log

The inbox file is a delivery mechanism. It is not an audit log or message history. Large inboxes (50+ messages) waste context window when agents load their full history.

- If message history is needed for debugging or auditing, use a separate append-only JSONL audit log
- Use `atm teams cleanup` to prune dead agent inboxes regularly
- `atm teams cleanup` deletes inbox unconditionally when agent is dead — no preservation

### 11. External Agents Can Corrupt Team-Lead Inbox State

If an external agent (e.g., arch-ctm running `atm read --timeout 600`) has the wrong ATM identity set, it will read **team-lead's inbox** instead of its own. `atm read` marks messages as read and updates the `last-seen` watermark — even when called by the wrong identity.

**Consequence**: Claude Code's auto-injection of teammate `SendMessage` replies relies on the `since-last-seen` cursor and `read: false` flags. If arch-ctm has already consumed and marked those messages read, Claude Code sees no new messages and silently drops the injection.

**Evidence**: `team-lead.json` stat updates immediately on send (delivery path works). The issue is state mutation, not delivery failure.

**Fix**: Always ensure `ATM_IDENTITY=<correct-name>` is set before running `atm read`. For arch-ctm, use `launch-arch-ctm.sh` which sets `ATM_IDENTITY=arch-ctm` automatically. For any long-running `atm read --timeout` listener, the identity MUST be correct or it will silently corrupt another agent's inbox state.

**System-level mitigation (Sprint B.1)**: `atm teams resume` resets the `last-seen` watermark for team-lead's inbox, which would recover from this corruption on next session start.

### 12. Claude Code Caches Team Config at Startup

Claude Code reads `leadSessionId` (and likely other team config) once at session start. Changes to `config.json` mid-session (e.g., patching `leadSessionId` via Python) take effect for `atm` CLI (which reads config fresh on each invocation) but **not** for Claude Code's internal message delivery. Teammate `SendMessage` replies will still route to the old dead session.

**Implication**: `atm teams resume` must be run **before** the Claude Code session starts, not after.

---

## Recommendations for `atm`

### Implemented (Current State)

1. **Offline delivery documented** in API docs ✅
2. **Use spawn prompts for task assignment** — more reliable than inbox queuing ✅
3. **`[PENDING ACTION]` tag pattern** — defensive measure for queued messages ✅
4. **`atm send` offline detection** — warns if recipient `isActive == false` ✅
5. **`--since-last-seen` cursor** — default for `atm read`, prevents inbox flooding ✅

### Planned (Sprint B.1)

1. **`atm teams resume <team>`**: Update `leadSessionId` from `CLAUDE_SESSION_ID`, set all Claude members `isActive=false` except team-lead, notify active members, output `TeamCreate` call. Run before every session start.
2. **`atm teams cleanup [team] [agent]`**: Remove dead members from `config.json` and delete their inbox files. Run after `resume` and after sprints complete. Single-agent form for ad-hoc cleanup.
3. **Daemon session tracking**: `SessionStart` hook → registry of `agent_name → {session_id, process_id, state}`. Authority on liveness for `resume` and `cleanup` liveness checks.
4. **CLAUDE.md startup sequence**: `atm teams resume` → `TeamCreate` → `atm teams cleanup`

### Long-term

1. **Delivery receipts**: Track whether a message was actually processed (not just read-marked)
2. **Retry/escalation**: If a queued message isn't acted on within N minutes of agent respawn, re-deliver or escalate
