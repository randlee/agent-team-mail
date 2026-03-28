# Phase BC Planning: Hook Runtime State + Gate Contracts

**Status**: Planned
**Primary docs**:
- `docs/requirements.md`
- `docs/claude-hook-strategy.md`
- `docs/observability/requirements.md`
- `docs/observability/architecture.md`
- `docs/agent-teams-hooks.md`

## Goal

Freeze the post-capture hook design before implementation so ATM does not repeat
the earlier state-drift and multi-authority problems.

Phase BC establishes:

- one canonical on-disk source of truth for per-agent hook state
- one normalized runtime state machine shared across Claude and future providers
- one single-process hook execution path with no daemon in the critical path for
  state or logging
- one mandatory structured logging contract for every hook invocation
- one sealed hook trait boundary for generic hooks plus an ATM-specific
  extension crate
- one explicit spawn/tool gating contract for fenced JSON validation

## Captured Claude Baseline

The harness work completed before BC produced enough evidence to freeze the
Claude baseline:

- `SessionStart.source` values confirmed locally:
  - captured: `startup`, `compact`
  - documented but not yet locally captured: `resume`, `clear`
- `PreCompact` is real and was captured.
- modern Claude spawn requests arrive as `PreToolUse` with
  `tool_name = "Agent"`, not `Task`.
- `PermissionRequest` was captured for at least:
  - `Write`
  - `Bash`
- `Stop` is the reliable observed transition back to normalized idle.
- `Notification(idle_prompt)` remained unresolved in local Haiku testing even
  after matcher and timeout corrections; it stays wired but cannot be treated as
  required evidence for BC implementation.
- `CLAUDE_PROJECT_DIR` is present at `SessionStart` hook execution time and is
  the authoritative project-root signal for Claude hook scripts.
- `SessionStart` stdin payload itself carries only `session_id` and `source`;
  it does not carry cwd or project-root fields.

## Design Rules

1. Hook state must have one source of truth.
2. Root/project identity must never be guessed from cwd.
3. Hook handlers must stay fast; no daemon hop is allowed for state or logging.
4. Hook logs are mandatory for every hook invocation initially.
5. Raw provider events and normalized runtime state are separate concepts.
6. ATM-specific behavior must live in a separate extension crate, not the
   generic hook core.

## Identity and Context Model

### Agent Identity

The unique live agent instance is identified by:

- `session_id`
- `active_pid`

That pair is the stable runtime identity for one running agent session.

### Project / Team Scope

The canonical root association field is:

- `project_root_dir`

For Claude, `project_root_dir` is sourced from `CLAUDE_PROJECT_DIR` at
`SessionStart`.

`project_root_dir` is mandatory resolved context. It must not be inferred from
cwd or any fallback heuristic. Once the association is established, all later
hook calls resolve it from the canonical persisted session record.

### Lineage

Parent/child linkage is captured explicitly:

- `parent_session_id` optional
- `parent_active_pid` optional

This supports named teammates, sub-agents, and worktree-spawned descendants
without relying on directory heuristics.

### ATM Extension

ATM data is optional generic extension data:

- `extensions.atm.atm_team`
- `extensions.atm.atm_identity`

ATM values are sourced from environment/config at startup and persisted onto the
canonical session record. They must not become a second authority for root or
identity resolution.

## Canonical Session-State File

### Storage Rules

- One JSON file per `session_id`
- Disk file is the source of truth; in-memory state is only a working copy
- Writes are atomic (`temp + rename`)
- No daemon cache is authoritative for hook-state correctness
- Session-state storage must use the standard ATM state root, not `/tmp`

### Canonical Schema

```json
{
  "schema_version": "v1",
  "provider": "claude",
  "session_id": "9e6e0d07-2f90-4b24-8f5a-5efcd4123456",
  "active_pid": 12345,
  "parent_session_id": null,
  "parent_active_pid": null,
  "project_root_dir": "/Users/randlee/Documents/github/agent-team-mail",
  "session_start_source": "startup",
  "agent_state": "starting",
  "state_revision": 1,
  "created_at": "2026-03-27T22:00:00Z",
  "updated_at": "2026-03-27T22:00:00Z",
  "ended_at": null,
  "last_hook_event": "SessionStart",
  "last_hook_event_at": "2026-03-27T22:00:00Z",
  "state_reason": "session_started",
  "extensions": {
    "atm": {
      "atm_team": "atm-dev",
      "atm_identity": "team-lead"
    }
  }
}
```

### Field Contract

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `schema_version` | string | yes | starts at `v1` |
| `provider` | string enum | yes | initial value `claude`; future values include `codex`, `gemini`, `cursor`, `opencode` |
| `session_id` | string | yes | logical session identifier |
| `active_pid` | integer | yes | long-lived agent process PID, not the hook subprocess PID |
| `parent_session_id` | string/null | yes | explicit lineage when spawned from another agent |
| `parent_active_pid` | integer/null | yes | parent long-lived PID when known |
| `project_root_dir` | string | yes | sourced from provider startup context; no cwd fallback |
| `session_start_source` | string enum | yes | `startup|resume|clear|compact` |
| `agent_state` | string enum | yes | normalized runtime state |
| `state_revision` | integer | yes | increments on every persisted transition |
| `created_at` | RFC3339 UTC string | yes | session record creation time |
| `updated_at` | RFC3339 UTC string | yes | last write time |
| `ended_at` | RFC3339 UTC string/null | yes | set when terminal state is persisted |
| `last_hook_event` | string | yes | raw hook event name |
| `last_hook_event_at` | RFC3339 UTC string | yes | raw hook event timestamp |
| `state_reason` | string | yes | stable machine-readable transition reason |
| `extensions.atm.atm_team` | string/null | yes | optional ATM team enrichment |
| `extensions.atm.atm_identity` | string/null | yes | optional ATM identity enrichment |

### Required Chaining Rule

BC must close the current ATM gap: after the initial `SessionStart`, the
session-state file must preserve the `session_id + active_pid + project_root_dir`
association so future hook calls do not depend on startup env repeating it.

## Normalized Agent-State Model

### Enum

The normalized runtime enum is:

- `starting`
- `busy`
- `awaiting_permission`
- `compacting`
- `idle`
- `ended`

This is a runtime enum, not typestate.

### Transition Table

| Raw event | Condition | New state | Notes |
| --- | --- | --- | --- |
| `SessionStart` | `source = startup` | `starting` | create new canonical session record |
| `SessionStart` | `source = resume` | `starting` | load existing record, update `active_pid` |
| `SessionStart` | `source = clear` | `starting` | new logical session, new record |
| `SessionStart` | `source = compact` | `starting` | same logical session after compaction |
| `PreToolUse(*)` | any tool | `busy` | includes `Bash`, `Agent`, future tools |
| `PermissionRequest` | approval needed | `awaiting_permission` | exact tool name remains in raw/log context |
| `PreCompact` | compaction begins | `compacting` | pre-restart transition |
| `Stop` | turn completed | `idle` | primary observed idle transition |
| `teammate_idle` | teammate runtime reports idle | `idle` | separate raw event that also maps to idle |
| `SessionEnd` | any reason | `ended` | set `ended_at` |

### State Rules

- Raw event names must not be reused as the internal state enum.
- `Stop` is the reliable turn-complete signal.
- `Notification(idle_prompt)` may be logged when present, but it is not the
  primary state transition to idle.
- `SessionStart(source="compact")` must not assume idle; a compacted session may
  continue working immediately after restart.

## Hook Execution Path

Every hook invocation follows one single path:

1. Parse raw JSON stdin and required env.
2. Resolve canonical context:
   - `session_id`
   - `active_pid`
   - `project_root_dir`
3. Load the session-state file if it exists, or create the initial working
   record when the event is `SessionStart`.
4. Build normalized hook context from raw event + persisted state.
5. Resolve handlers in deterministic order.
6. Execute handlers.
7. Collect handler return values.
8. Compute the normalized state transition.
9. Atomically write the updated session-state file.
10. Emit one structured hook log record (plus optional detailed handler records)
    through `sc-observability`.
11. Return final hook JSON to the runtime.

### Explicit Non-Goals

- No daemon request/response in the critical path for hook state.
- No second state authority in memory or temp files.
- No cwd-based fallback path resolution.

## Hook Logging Contract

Hook logging is mandatory for 100% of invocations in the initial BC
implementation.

The sink and health model must use the standardized `sc-observability`
contracts.

### Required Per-Invocation Fields

| Field | Required | Notes |
| --- | --- | --- |
| `ts` | yes | RFC3339 UTC |
| `source_binary` | yes | hook runtime binary/script adapter |
| `provider` | yes | initial value `claude` |
| `hook_event` | yes | raw event name |
| `session_id` | yes | canonical session identifier |
| `active_pid` | yes | canonical long-lived PID |
| `project_root_dir` | yes | canonical root association |
| `agent_state_before` | yes | normalized state before transition |
| `agent_state_after` | yes | normalized state after transition |
| `matched_handlers` | yes | ordered handler list |
| `handler_results` | yes | per-handler outcomes |
| `host_result` | yes | proceed / block / fail-open result |
| `state_revision` | yes | post-write revision |
| `atm_team` | no | ATM extension only |
| `atm_identity` | no | ATM extension only |
| `parent_session_id` | no | lineage when present |
| `parent_active_pid` | no | lineage when present |

### Example Log Record

```json
{
  "ts": "2026-03-27T22:05:00Z",
  "source_binary": "sc-hooks-runtime",
  "provider": "claude",
  "hook_event": "PermissionRequest",
  "session_id": "9e6e0d07-2f90-4b24-8f5a-5efcd4123456",
  "active_pid": 12345,
  "project_root_dir": "/Users/randlee/Documents/github/agent-team-mail",
  "agent_state_before": "busy",
  "agent_state_after": "awaiting_permission",
  "matched_handlers": [
    "sc-hooks-session-foundation",
    "sc-hooks-atm-extension"
  ],
  "handler_results": [
    { "handler": "sc-hooks-session-foundation", "result": "proceed" },
    { "handler": "sc-hooks-atm-extension", "result": "proceed" }
  ],
  "host_result": "proceed",
  "state_revision": 8,
  "atm_team": "atm-dev",
  "atm_identity": "team-lead"
}
```

## Trait and Crate Split

### `sc-hooks-core`

Owns:

- normalized context types
- canonical state-file schema types
- state-transition engine
- hook result types
- sealed hook trait definition

The hook trait in `sc-hooks-core` must be sealed so only `sc-hooks-sdk` can
provide base implementations. Unsealed traits would let external crates bypass
normalized-context and fail-open/fail-closed invariants.

### `sc-hooks-sdk`

Owns:

- provider adapters
- handler registration
- standard logging integration
- common validation helpers

### `sc-hooks-session-foundation`

Owns:

- `SessionStart`
- `SessionEnd`
- `PreCompact`
- normalized state persistence
- `project_root_dir` chaining

Fail posture: fail-open

### `sc-hooks-agent-spawn-gates`

Owns:

- `PreToolUse(Agent)` policy checks
- named-teammate vs background-agent rules
- fenced JSON validation for subagent launches

Fail posture: fail-closed

### `sc-hooks-tool-output-gates`

Owns:

- fenced JSON validation for tool outputs / tool-call payloads that require
  strict schema conformance

Fail posture: fail-closed

### `sc-hooks-atm-extension`

Owns:

- ATM identity/team enrichment
- ATM-specific relay fields
- teammate-idle mapping into normalized idle

Fail posture: fail-open

## Fenced JSON Spawn and Tool Policy

### Scope

When an agent or tool requires structured input, BC must support a strict
schema-driven validation path.

### Schema Discovery

For agent spawns, the schema must be defined in exactly one of these places:

1. in the agent prompt contract, or
2. as a sibling schema file with the same stem as the agent prompt

Example:

- `.claude/agents/scrum-master.md`
- `.claude/agents/scrum-master.schema.json`

### Validation Rules

- Require exactly one fenced `json` block when structured input is mandated.
- The JSON block must validate against the declared schema.
- On failure, the hook must block the launch.
- The failure message must say exactly why validation failed so the caller can
  retry without guesswork.

### Required Failure Shape

The block response must include:

- the blocked agent/tool name
- the schema source path
- one or more concrete validation failures

Example:

```text
BLOCKED: structured JSON input invalid for agent "scrum-master".
Schema: .claude/agents/scrum-master.schema.json
Errors:
- root.ticket_id: missing required property
- root.priority: expected one of [\"low\", \"medium\", \"high\"]
```

## Sprint Map

### BC.0 — Post-Capture Design Freeze

**Code / docs to write**:

- canonical session-state schema in docs
- transition table
- execution-path contract
- logging contract
- trait/crate boundaries

**Tests required**:

- doc review against captured Claude evidence
- cross-doc consistency review (`requirements`, `project-plan`,
  `claude-hook-strategy`, `observability`)

**Success criteria**:

- all implementation sprints can point to one stable hook design
- no remaining ambiguity about `project_root_dir`, identity, or state ownership

### BC.1 — Session Foundation

**Code to write**:

- `sc-hooks-core` state-file types and transition engine
- `sc-hooks-session-foundation`
- `SessionStart`, `SessionEnd`, `PreCompact`, `Stop` handling

**Tests required**:

- startup/resume/clear/compact state transition tests
- same-session compaction tests
- atomic-write and state-revision tests

**Success criteria**:

- canonical session file is created/updated/deleted correctly
- `project_root_dir` is chained after `SessionStart`
- `session_id + active_pid + project_root_dir` association survives all later
  hook calls

### BC.2 — Logging and Lineage

**Code to write**:

- hook logging integration through `sc-observability`
- lineage fields (`parent_session_id`, `parent_active_pid`)
- mandatory per-hook structured logging

**Tests required**:

- 100% hook logging coverage tests
- lineage persistence tests
- fail-open logging-degradation tests

**Success criteria**:

- every hook invocation emits a structured record
- state writes remain correct when logging degrades
- parent/child agent relationships are queryable from hook state and logs

### BC.3 — Agent Spawn and Tool Gates

**Code to write**:

- `sc-hooks-agent-spawn-gates`
- `sc-hooks-tool-output-gates`
- schema discovery and fenced JSON validation

**Tests required**:

- valid fenced JSON passes
- malformed JSON blocks with exact retryable errors
- wrong schema / missing required properties block deterministically
- named-teammate-required policies block background launches

**Success criteria**:

- structured-input agents and tools cannot launch with ambiguous payloads
- block messages are precise enough for immediate caller retry

### BC.4 — ATM Extension

**Code to write**:

- `sc-hooks-atm-extension`
- ATM team/identity enrichment
- teammate-idle normalization to `idle`
- ATM-specific relay/log enrichment that does not become a second state engine

**Tests required**:

- ATM env inheritance tests
- worktree-spawned child-agent linkage tests
- fail-open extension degradation tests

**Success criteria**:

- ATM fields enrich the canonical record without replacing generic identity
- teammate-idle integrates cleanly with normalized `agent_state`
- ATM behavior remains isolated from generic hook correctness

## Exit Criteria

1. Hook runtime state has one authoritative on-disk source of truth.
2. `project_root_dir` is captured once and chained for every later hook call.
3. Hook execution is single-process and daemon-free for state/logging.
4. Every hook invocation logs raw event, handlers, results, and state
   transition.
5. Generic hook behavior and ATM-specific behavior are separated cleanly.
6. Spawn/tool validation is schema-driven and retryable.
