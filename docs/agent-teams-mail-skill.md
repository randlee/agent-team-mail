# Agent Teams Mail Skill — Background & Findings

**Status**: Research / Early Design
**Date**: 2026-02-11

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

When an agent respawns, all historical inbox messages (including those queued while offline) appear in its conversation context and are marked `read: true`. However, the agent may not act on them:

- **Clean inbox** (few messages): Plain instructions are generally acted on
- **Noisy inbox** (many old messages): Plain instructions are treated as stale history and ignored
- **With call-to-action tag**: Instructions prefixed with `[PENDING ACTION]` or `[OFFLINE MESSAGE - Acknowledge and respond]` are reliably acted on regardless of inbox noise

The root cause is disambiguation — without a signal, the agent cannot distinguish "new task waiting for me" from "old task that was already handled by a previous instance."

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

This is simpler than tmux pane checks or heartbeat conventions. The `isActive` field is set by Claude Code's team infrastructure and doesn't require agent cooperation.

**Caveat**: Shutdown behavior is inconsistent — some agents are removed from the members array on shutdown, others remain with `isActive: false`. Both cases are handled by the detection logic above. The field could also be stale if a process crashes without clean shutdown, but this is an acceptable edge case for MVP.

### 6. Shutdown Removes Agent from Members (Inconsistently)

Agent shutdown via `shutdown_request` → `shutdown_response(approve: true)` sometimes removes the agent from `config.json` members array entirely, and sometimes leaves the entry with `isActive: false`. Both patterns were observed in the same team during testing. The inconsistency may relate to timing, concurrent operations, or whether a respawn occurred before cleanup completed.

## Recommendations for `atm`

### Short-term (Current MVP)

1. **Document the offline delivery behavior** in API docs (done)
2. **Use spawn prompts for task assignment**, not inbox queuing — this is the reliable path
3. **If queuing offline messages**, use `[PENDING ACTION]` tag pattern as a defensive measure

### Medium-term (Skill Enhancement)

1. **`atm send` offline detection**: Before sending, check `config.json` for recipient — if member not found or `isActive == false`, warn sender: "Agent X appears offline. Message will be queued."
2. **Auto-tag queued messages**: If recipient is detected offline, automatically prefix with `[PENDING ACTION - queued while offline at {timestamp}]`
3. **Delivery status field**: Add `delivery_status: "delivered" | "queued"` to inbox message schema
4. **`atm inbox --pending`**: Command to list messages that were queued while offline and may not have been processed
5. **`atm members` / `atm status` improvements**: Both commands already show `isActive` status. Ensure the labels are accurate — `isActive: false` means "offline/shut down", not "idle". Consider renaming display from "Active/Idle" to "Online/Offline" to avoid confusion with the agent's idle-but-alive state.

### Long-term (Daemon/Plugin)

1. **Delivery receipts**: Track whether a message was actually processed (not just read-marked)
2. **Retry/escalation**: If a queued message isn't acted on within N minutes of agent respawn, re-deliver or escalate
3. **Agent presence service**: Daemon maintains authoritative online/offline state for all agents
