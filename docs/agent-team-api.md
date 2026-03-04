# Agent Team API - Complete Reference

**API Version**: 0.1
**Date**: 2026-02-11
**Status**: Pre-Release (Experimental)

> **Reference Scope**
>
> This document mirrors the current Anthropic Agent Teams API/schema as observed today.
> It is a reference baseline for design, even if some content is not directly used in the
> current MVP scope.

> **Schema Baseline: Claude Code 2.1.39**
>
> All JSON schemas in this document were captured from Claude Code **v2.1.39**.
> The Agent Teams feature is **experimental and pre-release** — schemas may change
> without notice in future Claude Code versions. Any tool consuming these schemas
> should version-check against `claude --version` and handle unknown fields gracefully.
> See [`requirements.md`](./requirements.md) Section 3.1 for the versioning strategy.

---

## Overview

The Agent Team API provides programmatic access to create and manage teams of Claude agents. Teams enable multi-agent coordination through:

- **Team Management**: Create, configure, and delete teams
- **Agent Spawning**: Spawn specialized agents into teams
- **Task Coordination**: Create, assign, and track tasks with dependencies
- **Message System**: Send direct messages, broadcasts, and shutdown requests

---

## Table of Contents

1. [Team Management](#team-management)
2. [Agent Spawning](#agent-spawning)
3. [Task Management](#task-management)
4. [Message System](#message-system)
5. [Configuration Schemas](#configuration-schemas)
6. [Error Handling](#error-handling)
7. [Best Practices](#best-practices)

---

## Team Management

### TeamCreate

Create a new team for agent coordination.

**Method**: `TeamCreate`

**Parameters**:

```json
{
  "team_name": "string (required)",
  "description": "string (optional)",
  "agent_type": "string (optional, default: general-purpose)"
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `team_name` | string | Yes | Unique team identifier (alphanumeric, hyphens allowed) |
| `description` | string | No | Human-readable team purpose |
| `agent_type` | string | No | Agent type for team lead (e.g., "general-purpose", "Explore", "Plan") |

**Response**:

```json
{
  "team_name": "string",
  "team_file_path": "string",
  "lead_agent_id": "string"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `team_name` | string | Echo of input team name |
| `team_file_path` | string | Path to team config: `~/.claude/teams/{team_name}/config.json` |
| `lead_agent_id` | string | Format: `team-lead@{team_name}` |

**Example**:

```bash
TeamCreate:
  team_name: "backend-ci-team"
  description: "CI/CD monitoring and fix coordination"
  agent_type: "general-purpose"
```

**Response**:

```json
{
  "team_name": "backend-ci-team",
  "team_file_path": "/Users/randlee/.claude/teams/backend-ci-team/config.json",
  "lead_agent_id": "team-lead@backend-ci-team"
}
```

**Creates**:
- `~/.claude/teams/{team_name}/config.json` - Team configuration
- `~/.claude/teams/{team_name}/inboxes/` - Message inbox directory
- `~/.claude/tasks/{team_name}/` - Task list directory

**Constraints**:
- Team name must be unique
- Team name matches regex: `[a-z0-9\-]+`
- Maximum 100 teams per user

---

### TeamDelete

Delete a team and clean up associated resources.

**Method**: `TeamDelete`

**Parameters**:

```json
{
  "team_name": "string (required)"
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `team_name` | string | Yes | Team to delete |

**Response**:

```json
{
  "success": "boolean",
  "message": "string"
}
```

**Example**:

```bash
TeamDelete:
  team_name: "backend-ci-team"
```

**Removes**:
- `~/.claude/teams/{team_name}/` (entire directory tree)
- `~/.claude/tasks/{team_name}/` (entire directory tree)
- All associated inboxes, agent state, message history, and task records

**Destructive behavior**: `TeamDelete` is a full teardown — ALL team data is permanently deleted. This includes inbox files, task history, and team configuration. There is no partial cleanup of team data via the API. For backup and restore of team state, use `atm teams backup` and `atm teams restore` (see ATM CLI documentation). Any data not committed or copied elsewhere is lost.

**Constraints**:
- Team must exist
- Will fail if active members still running — shut down all agents first
- Cannot be undone

**When to use vs. agent shutdown**:
- To shut down a **single agent** without destroying the team, use `SendMessage` with `type: "shutdown_request"`. The team, task list, inboxes, and remaining agents are preserved.
- Use `TeamDelete` only when the **entire team** is no longer needed and all data has been preserved elsewhere.

---

### Team Backup and Restore (ATM CLI)

The `atm` CLI provides a file-layer backup and restore facility that complements `TeamDelete`.

#### `atm teams backup <team>`

Creates a timestamped snapshot of team state at `~/.claude/teams/.backups/<team>/<timestamp>/`:

```
.backups/<team>/<timestamp>/
├── config.json        ← copy of ~/.claude/teams/<team>/config.json
├── inboxes/           ← copy of all inbox files
│   └── <agent>.json
└── tasks/             ← copy of ~/.claude/tasks/<team>/ (if present)
    └── <task-id>.json
```

- Timestamp format: `YYYYMMDDTHHMMSSz` (ISO 8601 compact UTC)
- Auto-prunes to the last 5 backups per team (oldest removed first)
- Also triggered automatically at the start of `atm teams resume` (non-fatal: a backup failure does not abort the resume)

#### `atm teams restore <team>`

Restores team state from a backup:

- **Default**: restores from the latest backup (lexicographic sort of timestamp dirs)
- `--from <path>`: restore from a specific backup directory
- `--dry-run`: show what would be restored without writing any files
- `--skip-tasks`: restore config + inboxes only, skip task list restoration

**Invariants**:
- `leadSessionId` in `config.json` is **never** overwritten — the current session's lead ID is always preserved
- `team-lead` member entry is **never** restored from backup — only non-leader members are restored
- Restore is **idempotent**: members already present in the current config are not duplicated

**Relationship to TeamDelete**: `TeamDelete` permanently destroys all team data. Creating a backup before `TeamDelete` and restoring afterward is the supported recovery path.

---

## Agent Spawning

### Task (Spawn Agent)

Spawn a specialized agent into a team.

**Method**: `Task`

**Parameters**:

```json
{
  "subagent_type": "string (required)",
  "team_name": "string (required)",
  "name": "string (required)",
  "prompt": "string (optional)",
  "description": "string (optional)",
  "model": "string (optional, default: inherited)",
  "mode": "string (optional, default: default)"
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `subagent_type` | string | Yes | Agent capability type: `general-purpose`, `Explore`, `Plan`, etc. |
| `team_name` | string | Yes | Team to join (must exist) |
| `name` | string | Yes | Agent instance name (must be unique within team) |
| `prompt` | string | No | Custom prompt for agent specialization |
| `description` | string | No | Purpose/role of this agent |
| `model` | string | No | Model override: `claude-opus-4-6`, `claude-sonnet-4-5-20250929`, `claude-haiku-4-5-20251001` |
| `mode` | string | No | Permission mode: `default`, `plan`, `acceptEdits`, `delegate` |

**Response**:

```json
{
  "agent_id": "string",
  "name": "string",
  "team_name": "string",
  "agentType": "string",
  "status": "spawned"
}
```

**Example**:

```bash
Task:
  subagent_type: "general-purpose"
  team_name: "backend-ci-team"
  name: "ci-fix-agent"
  prompt: """
    You are a CI fix specialist. When notified of CI failures, you:
    1. Review the failure details
    2. Identify root cause
    3. Create and test fixes
    4. Commit and push solution

    Reference: docs/ci-fix-guidelines.md
  """
```

**Response**:

```json
{
  "agent_id": "ci-fix-agent@backend-ci-team",
  "name": "ci-fix-agent",
  "team_name": "backend-ci-team",
  "agentType": "general-purpose",
  "status": "spawned"
}
```

**Agent Updates**:
Adds to team config at `~/.claude/teams/{team_name}/config.json`:

```json
{
  "agentId": "ci-fix-agent@backend-ci-team",
  "name": "ci-fix-agent",
  "agentType": "general-purpose",
  "model": "claude-opus-4-6",
  "prompt": "You are a CI fix specialist...",
  "color": "blue",
  "planModeRequired": false,
  "joinedAt": 1770772206905,
  "tmuxPaneId": "%14",
  "cwd": "/Users/randlee/work",
  "subscriptions": [],
  "backendType": "tmux",
  "isActive": true
}
```

**Constraints**:
- Team must exist
- Agent name must be unique within team
- Maximum 50 agents per team
- Agents are long-lived (until shutdown)

---

## Agent Lifecycle & Message Delivery

### PID and Session Registration Semantics

Daemon liveness/state uses backend-aware PID registration. The canonical status
surface (`atm members`, `atm status`, `atm doctor`) is daemon-derived.

The PID shown in `atm doctor` output and stored in the daemon session registry is the
**agent session process PID** — the long-lived process running the agent (for example,
the `claude` or `codex` process itself). It is NOT the PID of the `atm` subprocess
that ran the registration command. For Claude Code agents this is obtained via
`os.getppid()` from hook scripts (the hook's parent is the stable agent process). For
external agents (Codex, Gemini) it is the process's own PID (`os.getpid()`) written
at self-registration time.

**Registration sources by backend type**:

| Backend | Registration mechanism | PID source |
|---------|----------------------|------------|
| Claude Code | `session_start` hook event | `os.getppid()` from hook script |
| Codex | Self-registration on `atm send` or explicit `atm register` | `os.getpid()` of codex process |
| Gemini | Self-registration on `atm send` or explicit `atm register` | `os.getpid()` of gemini process |

Codex and Gemini do not have a Claude Code hook system. They write their PID and
session ID directly to their roster entry in `config.json` when sending messages,
or via the explicit `atm register <team> <name>` command.

**Validation chain**:
1. **Register**: a candidate PID/session is provided by hook or sender self-registration.
2. **Registration-time backend validation**: daemon inspects process name for that PID and compares to expected backend tokens:

   | Backend | Expected `comm` / process name |
   |---------|-------------------------------|
   | Claude Code | `claude` |
   | Codex | `codex` (native binary — NOT `node`) |
   | Gemini | `node` AND full args contain `gemini` |

3. **Liveness check**: daemon validates process existence (`kill -0` / platform equivalent),
   then re-verifies process name against backend token (cross-validate). Detects PID reuse
   by unrelated processes that happened to acquire the same PID after the agent exited.
4. **Cross-validation at read time**: daemon re-checks process-name/backend alignment during
   status queries to detect PID reuse by unrelated processes.

**Mismatch behavior** (`WARN` finding in `atm doctor`):
- If PID is alive but process name does not match declared backend, daemon rejects/marks
  registration as invalid, emits a WARN log, and surfaces a WARN diagnostic in `atm doctor`.
- WARN details include: agent name, backend type, expected process name token(s), actual
  process name, and PID.

**Cross-validate WARN finding schema** (in `atm doctor --json` `findings[]` array):

```json
{
  "severity": "warn",
  "check": "pid_cross_validate",
  "code": "PID_PROCESS_MISMATCH",
  "message": "agent '<name>' PID <pid> is alive but process name '<actual>' does not match expected '<expected>' for backend '<backend>'"
}
```

**`ACTIVE_WITHOUT_SESSION` finding**: When an activity hint is present for a member but no
daemon state record exists (for example after an `atm send` self-registration was
interrupted before creating the daemon record), the daemon creates the missing daemon state
record alongside PID registration and emits an `ACTIVE_WITHOUT_SESSION` WARN finding in
`atm doctor` to surface the anomaly for operator awareness.

Notes:
- `isActive` remains an activity hint (`busy` vs `idle`) and is not treated as liveness truth.
- Team-scoped daemon state is authoritative; config fields are advisory inputs for reconciliation/registration.

### Shutdown Behavior

Daemon-managed shutdown uses a shutdown-first flow:
- Daemon sends a `shutdown_request` message to the target mailbox.
- Daemon waits for graceful shutdown up to configured timeout while monitoring PID/session liveness.
- If the agent does not exit by timeout, daemon force-terminates the process.

After termination is confirmed (already-dead or timeout+kill), daemon teardown cleanup is coupled:
- Remove member entry from `config.json`.
- Delete the agent inbox file (`inboxes/{name}.json`).

Mailbox deletion is not a shutdown signal and must not be used to terminate an active agent.

### Respawn Behavior

Spawning a new agent with the same `name` into the same team:
- Produces the same `agentId` (format: `{name}@{team_name}`, deterministic)
- Gets a **new `tmuxPaneId`** (new process handle)
- The member entry in `config.json` is updated (not duplicated)
- `joinedAt` and `prompt` are updated to reflect the new spawn
- The agent starts with **fresh context** — no memory of previous sessions
- When daemon teardown has completed, mailbox history for the terminated instance is removed
- The **spawn prompt** is the primary driver of initial behavior

### Routing Architecture

Message delivery is entirely **name-based**:

| Layer | Keyed by | Persists across respawn? |
|-------|----------|------------------------|
| Inbox file | `name` (`{name}.json`) | Yes — accumulates across lifetimes |
| Message fields | `from`/`to` names | Yes — no agentId in messages |
| Team membership | `agentId` (`name@team`) | Re-created with same value |
| Process handle | `tmuxPaneId` (`%4`, `%5`...) | No — new each spawn |

The `agentId` is an internal bookkeeping field. It does not appear in inbox files or message routing. All routing uses agent `name`.

### Message Delivery to Offline Agents

`SendMessage` to a shut-down agent **succeeds silently**:
- The message is written to the agent's inbox file (`{name}.json`) with `read: false`
- No error or warning is returned to the sender
- Messages accumulate in the inbox file indefinitely

### Queued Message Processing on Respawn

When an agent is respawned (same name), behavior depends on teardown state:
- If prior instance teardown completed, the new agent starts with an empty mailbox.
- If prior teardown has not completed yet, stale messages may remain until daemon reconciliation finishes.

### Offline Queuing Pattern (opt-in)

By default, `atm send` delivers messages without any prefix — no call-to-action tag is added.

To add a call-to-action prefix for messages to offline agents, use `--offline-action`:

```bash
atm send <agent> "message" --offline-action "[PENDING ACTION - execute when online]"
```

or configure a default in `.atm.toml`:

```toml
[send]
offline_action = "[PENDING ACTION - execute when online]"
```

With a tag, the pattern has been 100% reliable in testing. Without one, the message is still delivered; the tag is purely a hint to the recipient.

### `atm doctor --json` Output Contract

`atm doctor --json` returns a stable top-level report object with:
- `summary`
- `findings`
- `recommendations`
- `log_window`
- `env_overrides`

`env_overrides` fields (present only when the env var is set to a non-empty value):
- `atm_home`: `{ "source": "env", "value": "<ATM_HOME>" }`
- `atm_team`: `{ "source": "env", "value": "<ATM_TEAM>" }`
- `atm_identity`: `{ "source": "env", "value": "<ATM_IDENTITY>" }`

Change-control note:
- Changed in Phase Y: `env_overrides` was added as a first-class top-level JSON
  field for diagnostics triage.

---

## Task Management

### TaskCreate

Create a task for team coordination.

**Method**: `TaskCreate`

**Parameters**:

```json
{
  "subject": "string (required)",
  "description": "string (required)",
  "activeForm": "string (optional)",
  "metadata": "object (optional)"
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `subject` | string | Yes | Brief imperative title (e.g., "Fix CI failure in backend") |
| `description` | string | Yes | Detailed requirements and acceptance criteria |
| `activeForm` | string | No | Present continuous shown while `in_progress` (e.g., "Fixing CI failure") |
| `metadata` | object | No | Custom key-value pairs for tracking |

**Response**:

```json
{
  "taskId": "string",
  "subject": "string",
  "description": "string",
  "status": "pending",
  "owner": null,
  "created_at": "string",
  "blockedBy": [],
  "blocks": []
}
```

**Example**:

```bash
TaskCreate:
  subject: "Fix authentication timeout in login flow"
  description: |
    Investigate and fix authentication timeout issues reported in CI.

    Acceptance Criteria:
    - Identify root cause of timeout
    - Implement fix
    - Add unit test for edge case
    - Verify fix doesn't break existing tests
    - Update documentation if needed
  activeForm: "Fixing authentication timeout"
  metadata:
    priority: "high"
    component: "auth"
    affected_endpoints: ["POST /login"]
```

**Response**:

```json
{
  "taskId": "1",
  "subject": "Fix authentication timeout in login flow",
  "description": "Investigate and fix...",
  "status": "pending",
  "owner": null,
  "created_at": "2026-02-11T14:30:00Z",
  "blockedBy": [],
  "blocks": []
}
```

**Constraints**:
- Subject max 200 characters
- Description max 5000 characters
- Metadata values must be JSON serializable
- Maximum 1000 tasks per team

---

### TaskUpdate

Update task status, ownership, and dependencies.

**Method**: `TaskUpdate`

**Parameters**:

```json
{
  "taskId": "string (required)",
  "status": "enum (optional)",
  "owner": "string (optional)",
  "subject": "string (optional)",
  "description": "string (optional)",
  "activeForm": "string (optional)",
  "addBlockedBy": "array (optional)",
  "addBlocks": "array (optional)",
  "metadata": "object (optional)"
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `taskId` | string | Yes | Task identifier |
| `status` | enum | No | `pending`, `in_progress`, `completed`, `deleted` |
| `owner` | string | No | Agent name to assign task |
| `subject` | string | No | Update task title |
| `description` | string | No | Update task description |
| `activeForm` | string | No | Update progress message |
| `addBlockedBy` | array | No | Task IDs that must complete first |
| `addBlocks` | array | No | Task IDs that depend on this one |
| `metadata` | object | No | Merge metadata keys (set value to null to delete) |

**Response**:

```json
{
  "taskId": "string",
  "subject": "string",
  "status": "string",
  "owner": "string or null",
  "blockedBy": "array",
  "blocks": "array",
  "updated_at": "string"
}
```

**Examples**:

**Assign to Agent**:
```bash
TaskUpdate:
  taskId: "1"
  owner: "ci-fix-agent"
  status: "pending"
```

**Start Work**:
```bash
TaskUpdate:
  taskId: "1"
  status: "in_progress"
```

**Complete Task**:
```bash
TaskUpdate:
  taskId: "1"
  status: "completed"
```

**Add Dependency** (Task 2 blocked by Task 1):
```bash
TaskUpdate:
  taskId: "2"
  addBlockedBy: ["1"]
```

**Status Transitions**:

```
pending → in_progress → completed
  ↓
deleted (any state)
```

**Constraints**:
- Task must exist
- Status must be valid enum
- Owner must be valid agent name in team
- Cannot create circular dependencies
- Metadata merge (not replace)

---

### TaskGet

Retrieve full task details.

**Method**: `TaskGet`

**Parameters**:

```json
{
  "taskId": "string (required)"
}
```

**Response**:

```json
{
  "taskId": "string",
  "subject": "string",
  "description": "string",
  "activeForm": "string",
  "status": "string",
  "owner": "string or null",
  "created_at": "string",
  "updated_at": "string",
  "blockedBy": ["array of taskIds"],
  "blocks": ["array of taskIds"],
  "metadata": "object"
}
```

**Example**:

```bash
TaskGet:
  taskId: "1"
```

**Response**:

```json
{
  "taskId": "1",
  "subject": "Fix authentication timeout in login flow",
  "description": "Investigate and fix...",
  "activeForm": "Fixing authentication timeout",
  "status": "in_progress",
  "owner": "ci-fix-agent",
  "created_at": "2026-02-11T14:30:00Z",
  "updated_at": "2026-02-11T14:35:00Z",
  "blockedBy": [],
  "blocks": ["2", "3"],
  "metadata": {
    "priority": "high",
    "component": "auth"
  }
}
```

---

### TaskList

List all tasks in team (with filtering).

**Method**: `TaskList`

**Parameters**: None (returns all tasks for current team)

**Response**:

```json
{
  "tasks": [
    {
      "id": "string",
      "subject": "string",
      "status": "string",
      "owner": "string or null",
      "blockedBy": ["array"],
      "blocks": ["array"]
    }
  ],
  "total": "number"
}
```

**Example Response**:

```json
{
  "tasks": [
    {
      "id": "1",
      "subject": "Fix authentication timeout in login flow",
      "status": "pending",
      "owner": "ci-fix-agent",
      "blockedBy": [],
      "blocks": ["2"]
    },
    {
      "id": "2",
      "subject": "Update documentation with fix",
      "status": "pending",
      "owner": null,
      "blockedBy": ["1"],
      "blocks": []
    }
  ],
  "total": 2
}
```

---

## Message System

### SendMessage

Send messages between team members.

**Method**: `SendMessage`

**Parameters**:

```json
{
  "type": "enum (required)",
  "recipient": "string (optional)",
  "content": "string (optional)",
  "summary": "string (optional)",
  "request_id": "string (optional)",
  "approve": "boolean (optional)"
}
```

#### Message Types

**Type 1: Direct Message**

```json
{
  "type": "message",
  "recipient": "string (required)",
  "content": "string (required)",
  "summary": "string (required, 5-10 words)"
}
```

Send to single agent.

**Example**:

```bash
SendMessage:
  type: "message"
  recipient: "ci-fix-agent"
  content: "CI failure detected in backend tests. Review the failure details and implement a fix."
  summary: "CI failure detected in backend"
```

**Type 2: Broadcast**

```json
{
  "type": "broadcast",
  "content": "string (required)",
  "summary": "string (required, 5-10 words)"
}
```

Send to ALL team members.

**Example**:

```bash
SendMessage:
  type: "broadcast"
  content: "Critical security update released. Please review and deploy by EOD."
  summary: "Critical security update - deploy by EOD"
```

**Type 3: Shutdown Request**

```json
{
  "type": "shutdown_request",
  "recipient": "string (required)",
  "content": "string (required)"
}
```

Request agent to shut down.

**Example**:

```bash
SendMessage:
  type: "shutdown_request"
  recipient: "ci-fix-agent"
  content: "Task complete. Please wrap up and prepare for shutdown."
```

**Type 4: Shutdown Response**

```json
{
  "type": "shutdown_response",
  "request_id": "string (required)",
  "approve": "boolean (required)",
  "content": "string (optional)"
}
```

Respond to shutdown request (agent use only).

**Example**:

```bash
SendMessage:
  type: "shutdown_response"
  request_id: "abc-123"
  approve: true
  content: "All tasks complete. Ready to shut down."
```

**Type 5: Plan Approval Response**

```json
{
  "type": "plan_approval_response",
  "request_id": "string (required)",
  "recipient": "string (required)",
  "approve": "boolean (required)",
  "content": "string (optional)"
}
```

Approve or reject agent's implementation plan.

**Response**:

```json
{
  "success": "boolean",
  "message": "string",
  "recipients": ["array of agent names"],
  "routing": {
    "sender": "string",
    "target": "string",
    "summary": "string"
  }
}
```

**Example Response**:

```json
{
  "success": true,
  "message": "Message sent successfully",
  "recipients": ["ci-fix-agent"],
  "routing": {
    "sender": "team-lead",
    "target": "ci-fix-agent",
    "summary": "CI failure detected in backend"
  }
}
```

**Constraints**:
- Broadcast limited to once per 5 seconds per team
- Message content max 10,000 characters
- Summary max 100 characters
- Recipient must be valid agent in team

---

## Configuration Schemas

### Team Configuration

**File**: `~/.claude/teams/{team_name}/config.json`

**Root Level Schema**:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Team name (matches directory name) |
| `description` | string | No | Human-readable team purpose |
| `createdAt` | number | Yes | Unix timestamp in milliseconds |
| `leadAgentId` | string | Yes | Format: `team-lead@{team_name}` |
| `leadSessionId` | string | Yes | UUID of session that created team |
| `members` | array | Yes | Array of agent member objects |

**Agent Member Schema**:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `agentId` | string | Yes | Format: `{name}@{team_name}` (unique identifier) |
| `name` | string | Yes | Agent instance name (unique within team) |
| `agentType` | string | Yes | Agent capability type |
| `model` | string | Yes | Claude model identifier |
| `prompt` | string | No | Custom prompt for specialization (null for team-lead) |
| `color` | string | No | UI color code (blue, green, yellow, etc.) |
| `planModeRequired` | boolean | No | Whether plan mode is required (default: false) |
| `joinedAt` | number | Yes | Unix timestamp when agent joined |
| `tmuxPaneId` | string | No | Terminal pane ID (empty string if no terminal) |
| `cwd` | string | Yes | Current working directory of agent |
| `subscriptions` | array | No | Notification subscriptions (usually empty) |
| `backendType` | string | No | Backend type (e.g., "tmux", empty if not running) |
| `isActive` | boolean | No | Activity/busy hint (recent work signal), not a liveness indicator |

**Complete Example** (from test-team):

```json
{
  "name": "test-team",
  "description": "Test team for agent coordination and workflow demonstration",
  "createdAt": 1770765919076,
  "leadAgentId": "team-lead@test-team",
  "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
  "members": [
    {
      "agentId": "team-lead@test-team",
      "name": "team-lead",
      "agentType": "general-purpose",
      "model": "claude-haiku-4-5-20251001",
      "joinedAt": 1770765919076,
      "tmuxPaneId": "",
      "cwd": "/Users/randlee/Documents/github/agent-teams-test/test-workspace",
      "subscriptions": []
    },
    {
      "agentId": "haiku-poet-1@test-team",
      "name": "haiku-poet-1",
      "agentType": "general-purpose",
      "model": "claude-opus-4-6",
      "prompt": "You are a creative haiku poet. Wait for the team lead's broadcast message with a haiku composition request, then compose and share your best haiku with the team. Make it meaningful and poetic.",
      "color": "blue",
      "planModeRequired": false,
      "joinedAt": 1770772206905,
      "tmuxPaneId": "%14",
      "cwd": "/Users/randlee/Documents/github/agent-teams-test/test-workspace",
      "subscriptions": [],
      "backendType": "tmux",
      "isActive": false
    },
    {
      "agentId": "haiku-poet-2@test-team",
      "name": "haiku-poet-2",
      "agentType": "general-purpose",
      "model": "claude-opus-4-6",
      "prompt": "You are a nature haiku specialist. Wait for the team lead's broadcast message with a haiku composition request, then compose and share a haiku about nature or software development. Make it vivid and memorable.",
      "color": "green",
      "planModeRequired": false,
      "joinedAt": 1770772207583,
      "tmuxPaneId": "%15",
      "cwd": "/Users/randlee/Documents/github/agent-teams-test/test-workspace",
      "subscriptions": [],
      "backendType": "tmux",
      "isActive": true
    },
    {
      "agentId": "haiku-poet-3@test-team",
      "name": "haiku-poet-3",
      "agentType": "general-purpose",
      "model": "claude-opus-4-6",
      "prompt": "You are a tech haiku specialist. Wait for the team lead's broadcast message with a haiku composition request, then compose and share a haiku about agents, teams, or AI. Make it clever and insightful.",
      "color": "yellow",
      "planModeRequired": false,
      "joinedAt": 1770772208362,
      "tmuxPaneId": "%16",
      "cwd": "/Users/randlee/Documents/github/agent-teams-test/test-workspace",
      "subscriptions": [],
      "backendType": "tmux",
      "isActive": true
    }
  ]
}
```

### Inbox Message Schema

**File**: `~/.claude/teams/{team_name}/inboxes/{agent_name}.json`

**Message Object**:

```json
{
  "from": "string (sender agent name or 'team-lead')",
  "text": "string (message content, markdown supported)",
  "timestamp": "string (ISO 8601 UTC)",
  "read": "boolean",
  "summary": "string (optional, brief summary)"
}
```

**Field Notes**:

- **Team Lead Member**: First member has empty/null `prompt`, `color`, `tmuxPaneId`, and no `backendType`
- **Spawned Agents**: Have `prompt`, `color`, `tmuxPaneId`, and `backendType` populated
- **`model`**: Different agents can use different models (e.g., team-lead uses haiku, agents use opus)
- **`isActive`**: activity signal only (true=busy/sending, false=idle); NOT a liveness indicator — use daemon session state for liveness
- **`prompt`**: Where specialized instructions are stored (can be long multi-line text)
- **`color`**: UI color for team dashboard (optional but recommended)

---

### Inbox File Format

**File**: `~/.claude/teams/{team_name}/inboxes/{agent_name}.json`

**Inbox File** (array of messages):

```json
[
  {
    "from": "team-lead",
    "text": "CI failure detected in backend tests",
    "timestamp": "2026-02-11T14:30:00.000Z",
    "read": false,
    "summary": "CI failure detected"
  },
  {
    "from": "ci-fix-agent",
    "text": "Acknowledged. Beginning investigation.",
    "timestamp": "2026-02-11T14:30:15.000Z",
    "read": true,
    "summary": "Investigation started"
  }
]
```

### Task Schema

**Storage**: Task files in `~/.claude/tasks/{team_name}/`

**Task Object**:

```json
{
  "taskId": "string",
  "subject": "string",
  "description": "string",
  "activeForm": "string",
  "status": "enum (pending|in_progress|completed|deleted)",
  "owner": "string or null (agent name)",
  "created_at": "string (ISO 8601)",
  "updated_at": "string (ISO 8601)",
  "blockedBy": ["array of taskIds"],
  "blocks": ["array of taskIds"],
  "metadata": "object (custom key-value)"
}
```

---

### Claude Code Settings (`settings.json`)

Claude Code uses a layered settings system. The `settings.json` file is the official mechanism for configuration across user, project, and local scopes, with managed policies and CLI overrides taking precedence.

Reference: https://docs.anthropic.com/en/docs/claude-code/hooks (redirects to https://code.claude.com/docs/en/hooks)

**Settings file locations (by scope)**:
- User: `~/.claude/settings.json` 
- Project (shared): `.claude/settings.json` 
- Local (personal, gitignored): `.claude/settings.local.json` 
- Managed (enterprise policy): `managed-settings.json` in system locations (macOS `/Library/Application Support/ClaudeCode/`, Linux/WSL `/etc/claude-code/`, Windows `C:\Program Files\ClaudeCode\`) 

**Settings precedence (highest → lowest)**:
1. Managed (cannot be overridden)
2. CLI arguments
3. Local (`.claude/settings.local.json`)
4. Project (`.claude/settings.json`)
5. User (`~/.claude/settings.json`)


**Schema reference**:
```json
{
  "$schema": "https://json.schemastore.org/claude-code-settings.json"
}
```


**Example settings.json**:
```json
{
  "$schema": "https://json.schemastore.org/claude-code-settings.json",
  "permissions": {
    "allow": ["Bash(npm run lint)", "Read(~/.zshrc)"],
    "deny": ["Bash(curl *)", "Read(./secrets/**)"]
  },
  "env": {
    "CLAUDE_CODE_ENABLE_TELEMETRY": "1"
  }
}
```


**Core settings fields (non-exhaustive)**:
- `permissions`: rule lists (e.g., `allow`, `deny`, `ask`) controlling tool access and file reads.
- `env`: environment variables applied to sessions.
- Additional keys exist (hooks, model, status line, plugin settings, etc.) and are defined by the official JSON schema.


**Implementation guidance**:
- Consumers must accept and preserve unknown settings fields.
- The official JSON schema is the source of truth for the full settings surface.


## Error Handling

### Error Responses

All API methods return error information in standard format:

```json
{
  "success": false,
  "error": "string (error type)",
  "message": "string (human-readable message)",
  "details": "object (optional, additional context)"
}
```

### Error Types

| Error | Status | Description |
|-------|--------|-------------|
| `team_not_found` | 404 | Team does not exist |
| `agent_not_found` | 404 | Agent not in team |
| `task_not_found` | 404 | Task does not exist |
| `team_already_exists` | 409 | Team name already taken |
| `agent_already_exists` | 409 | Agent name already in team |
| `invalid_status` | 400 | Status transition not allowed |
| `circular_dependency` | 400 | Circular task dependency detected |
| `permission_denied` | 403 | Insufficient permissions |
| `rate_limit` | 429 | Too many requests |
| `internal_error` | 500 | Server error |

**Example Error Response**:

```json
{
  "success": false,
  "error": "team_not_found",
  "message": "Team 'backend-ci-team' does not exist",
  "details": {
    "team_name": "backend-ci-team",
    "available_teams": ["test-team", "nuget-team"]
  }
}
```

---

## Best Practices

### 1. Team Naming Convention

Use repo name as team name for automatic discovery:

```yaml
# ✅ GOOD
team_name: "backend"      # Matches repo name

# ❌ AVOID
team_name: "my-special-backend-team"
```

### 2. Agent Naming Convention

Use descriptive instance names:

```bash
# ✅ GOOD
name: "ci-fix-agent"
name: "code-reviewer"
name: "test-runner"

# ❌ AVOID
name: "agent1"
name: "worker"
```

### 3. Task Dependencies

Always use dependencies for sequential workflows:

```bash
# ✅ GOOD - Sequential with dependencies
TaskCreate: subject="Design"
TaskCreate: subject="Implement"
TaskUpdate: taskId="2", addBlockedBy=["1"]
TaskCreate: subject="Test"
TaskUpdate: taskId="3", addBlockedBy=["2"]

# ❌ AVOID - No dependency tracking
TaskCreate: subject="Design"
TaskCreate: subject="Implement"
TaskCreate: subject="Test"
# Hope agent does them in order
```

### 4. Message Content

Keep inbox messages concise, reference external reports:

```bash
# ✅ GOOD - Minimal inbox, reference local file
SendMessage:
  type: "message"
  recipient: "ci-fix-agent"
  content: "CI failure in backend. Details: /repo/temp/ci-failures/report.md"
  summary: "CI failure in backend"

# ❌ AVOID - Bloats inbox with details
SendMessage:
  type: "message"
  recipient: "ci-fix-agent"
  content: "Long detailed failure report... [5000 chars]"
```

### 5. Graceful Shutdown

Shut down individual agents vs. entire teams:

```bash
# ✅ Shut down ONE agent (team stays alive)
SendMessage:
  type: "shutdown_request"
  recipient: "sprint-1-dev"
  content: "Sprint complete, shutting you down"
# Wait for shutdown_response
# Team and tasks are preserved; the terminated agent's mailbox is cleaned up by daemon teardown

# ✅ Shut down ENTIRE team (all data removed)
# First: shutdown all remaining agents
SendMessage:
  type: "shutdown_request"
  recipient: "agent-1"
SendMessage:
  type: "shutdown_request"
  recipient: "agent-2"
# Wait for all shutdown_responses
# Then: destroy team (removes ALL files — config, tasks, inboxes)
TeamDelete

# ❌ AVOID - TeamDelete with active agents
TeamDelete  # Fails if agents still running
```

**Important**: `TeamDelete` permanently removes all team data. Only use it when the team is fully done and all valuable data (task history, messages) has been preserved elsewhere.

### 6. State Tracking

Use metadata for tracking important state:

```bash
# ✅ GOOD - Metadata for tracking
TaskCreate:
  subject: "Fix backend timeout"
  metadata:
    priority: "critical"
    component: "auth"
    affected_users: 150
    incident_id: "INC-1234"

# Retrieve and use
TaskGet: taskId="1"
# Can filter/report on metadata
```

### 7. Concurrent Operations

Avoid simultaneous writes to same task:

```bash
# ✅ GOOD - Coordinate updates
Agent1: TaskUpdate: taskId="1", status="in_progress"
Agent2: Wait for Agent1 to complete
Agent2: TaskUpdate: taskId="1", status="completed"

# ❌ AVOID - Race conditions
Agent1: TaskUpdate: taskId="1", status="in_progress"
Agent2: TaskUpdate: taskId="1", status="in_progress"  # Conflict
```

### 8. File Watcher Inbox Messages

When manually writing to inbox files (file watcher pattern):

```json
{
  "from": "external-system",
  "text": "Message content",
  "timestamp": "2026-02-11T14:30:00.000Z",
  "read": false,
  "summary": "Concise summary"
}
```

Use atomic writes to prevent corruption:

```python
# Write to temp, then move (atomic)
with open(inbox_file + ".tmp", "w") as f:
    json.dump(messages, f)
os.rename(inbox_file + ".tmp", inbox_file)
```

---

## Conventions

### Team Name = Repo Name

Recommended convention for easy discovery:

```bash
# When in repo directory
cd /Users/randlee/backend

# Team name matches repo name
TeamCreate: team_name="backend"

# Agents know team from directory
Task: team_name="backend", name="ci-fix-agent"
```

### Agent Name Prefix Convention

Use type prefix for easy identification:

```
ci-fix-agent        # CI failure fixer
code-reviewer       # Code review specialist
test-runner         # Test execution
pm-design           # Project manager for design
pm-implementation   # Project manager for implementation
qa-tester           # QA and testing
```

### Task ID Format

Task IDs are sequential strings:

```
"1", "2", "3", ...
```

Reference in dependencies as strings, not integers.

---

## Rate Limits

- Team creation: 10 teams per hour per user
- Agent spawning: 50 agents per hour per team
- Task creation: 1000 tasks per day per team
- Message sending: 1 broadcast per 5 seconds per team
- API calls: 1000 per minute per user

---

## Changelog

### Version 1.0 (2026-02-11)
- Initial API documentation
- All core methods documented
- Configuration schemas included
- Best practices and conventions

---

**Document Version**: 1.0
**Last Updated**: 2026-02-11
**Maintained By**: Claude
