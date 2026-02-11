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

### 5. No Online/Offline Detection Mechanism

The Claude Code Agent Teams API provides no reliable way to determine if an agent is currently active:

- `isActive` in `config.json` is inconsistently maintained (sometimes agents are removed from members on shutdown, sometimes they remain with `isActive: false`)
- `tmuxPaneId` could be checked for liveness (is the pane still running?), but this is backend-specific and fragile
- There is no API call like "is agent X online?" or "list active agents"

**This is the hard problem for `atm`**: To warn senders about offline recipients or auto-tag messages for deferred delivery, `atm` needs to detect online/offline state. Possible approaches:

- **tmux pane check**: `tmux has-session` / `tmux list-panes` to verify the paneId is alive. Backend-specific, fragile.
- **Heartbeat convention**: Active agents periodically write a timestamp to a known location. Stale timestamp = offline. Requires agent cooperation.
- **isActive field**: Read `config.json` and check `isActive`. Unreliable due to inconsistent cleanup.
- **Process check**: Verify the agent's process is running. Platform-specific.
- **Accept uncertainty**: Don't detect — always queue, always tag. Let the recipient sort it out on respawn.

### 6. Shutdown Removes Agent from Members (Usually)

Agent shutdown via `shutdown_request` → `shutdown_response(approve: true)` typically removes the agent from `config.json` members array. However, this behavior was inconsistent in testing — some agents remained with `isActive: false`. The inconsistency may relate to timing, concurrent operations, or the respawn happening before cleanup completes.

## Recommendations for `atm`

### Short-term (Current MVP)

1. **Document the offline delivery behavior** in API docs (done)
2. **Use spawn prompts for task assignment**, not inbox queuing — this is the reliable path
3. **If queuing offline messages**, use `[PENDING ACTION]` tag pattern as a defensive measure

### Medium-term (Skill Enhancement)

1. **Auto-detect offline recipients**: Best-effort check of `isActive` + `tmuxPaneId` liveness before sending
2. **Warn sender**: "Agent X appears offline. Message will be queued." (not a hard block — still deliver)
3. **Auto-tag queued messages**: If recipient is detected offline, automatically prefix with `[PENDING ACTION - queued while offline at {timestamp}]`
4. **Delivery status field**: Add `delivery_status: "delivered" | "queued"` to inbox message schema
5. **`atm inbox --pending`**: Command to list messages that were queued while offline and may not have been processed

### Long-term (Daemon/Plugin)

1. **Delivery receipts**: Track whether a message was actually processed (not just read-marked)
2. **Retry/escalation**: If a queued message isn't acted on within N minutes of agent respawn, re-deliver or escalate
3. **Agent presence service**: Daemon maintains authoritative online/offline state for all agents
