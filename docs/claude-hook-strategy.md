# Claude Hook Strategy

## Purpose

Define the Claude-side hook strategy for ATM lifecycle/state tracking, spawn
policy enforcement, and identity correlation.

This document is the source of truth for Claude hook behavior and mapping.
`docs/requirements.md` should keep only stable invariants and reference this
document for hook-specific flow details.

The clean post-capture hook-runtime redesign now lives in `schook`. This ATM
document remains the current-ATM behavior reference and should not be treated
as the redesign authority.

## Primary References

- [agent-teams-hooks.md](agent-teams-hooks.md)
- [requirements.md](requirements.md)
- [settings.json](../.claude/settings.json)
- [scripts/](../.claude/scripts/)

## Strategy Goals

- Keep the canonical hook session-state file as the source of truth for
  hook-runtime state.
- Use hooks for fast lifecycle/activity updates with no daemon in the critical
  path for state or logging.
- Fail-open for observability hooks; fail-closed only for policy gates.
- Keep local scripts and installed scripts behaviorally identical.
- Treat `CLAUDE_PROJECT_DIR` as the authoritative project-root signal for Claude
  hook execution. `SessionStart` payload stdin does not carry cwd/project-root
  fields; root association must be established from env and then persisted as
  `project_root_dir`.

## Hook Surface and Responsibility

| Claude Hook | ATM Purpose | Strategy |
| --- | --- | --- |
| `SessionStart` | create or refresh canonical hook session state | Create/update state record with `session_id`, `active_pid`, `project_root_dir`, and `session_start_source` |
| `SessionEnd` | explicit session termination signal | Transition canonical state to `ended` and set `ended_at` |
| `PreCompact` | pre-restart compaction signal | Transition canonical state to `compacting` before restart |
| `PreToolUse(Agent)` | spawn policy enforcement | Block unauthorized/unsafe spawn calls with actionable errors |
| `PreToolUse(Bash)` | CLI identity correlation setup | Write per-invocation identity context for `atm` CLI |
| `PostToolUse(Bash)` | cleanup identity artifacts | Remove per-invocation identity context |
| `TeammateIdle` | low-latency idle transition | Normalize to `idle`; ATM extension may relay asynchronously for compatibility |
| `PermissionRequest` | blocked-on-approval activity state | Transition canonical state to `awaiting_permission`; optionally enrich ATM state/logging |
| `Stop` | turn-complete activity transition | Transition canonical state to `idle` |
| `Notification(idle_prompt)` | optional idle heartbeat | Keep wired when available, but do not require it for the primary idle transition |

## Lifecycle Mapping Strategy

Current ATM lifecycle notes:

- `SessionStart(startup|resume|clear|compact)` -> `starting`
- `PreToolUse(*)` -> `busy`
- `PermissionRequest` -> `awaiting_permission`
- `PreCompact` -> `compacting`
- `Stop` -> `idle`
- `TeammateIdle` -> `idle`
- `SessionEnd` -> `ended`

`Stop` is the primary observed transition back to idle. `Notification` may
still be logged when available, but it is not the required state transition.

## Deployment Strategy

- Project hooks in `.claude/settings.json` for repo-specific policy and relay behavior.
- Global hooks in `~/.claude/settings.json` for lifecycle events that must follow every Claude session.
- `atm init` is responsible for installing/updating hook registrations and scripts.
- The BC implementation must keep the generated hook configuration aligned with
  the real Claude surface names (`Agent`, `PreCompact`, `PermissionRequest`,
  etc.), not historical local naming drift.

## Behavior Rules

- Spawn-gate hook is fail-closed (`exit 2` on policy violation).
- Relay hooks are fail-open (`exit 0` on errors).
- Hook-reported PID must represent the long-lived session process (`os.getppid()` in hook subprocess).
- Hook logic must not infer liveness from activity fields (`isActive` means busy, not alive).
- Hook logic must not derive `project_root_dir` from cwd.
- Hook logging is mandatory for 100% of invocations in the initial BC design.

## State and Data Ownership

- The canonical session-state file owns hook-runtime identity and normalized
  state transitions.
- Hooks provide raw event signals plus provider metadata for that file.
- `project_root_dir` is sourced from `CLAUDE_PROJECT_DIR` at `SessionStart` and
  chained from persisted state after that.
- ATM extension data (`atm_team`, `atm_identity`) enriches the canonical record
  without replacing generic identity or root resolution.
- Current ATM gap: the SessionStart session file does not yet persist the
  authoritative `CLAUDE_PROJECT_DIR` / project-root association alongside
  `session_id` + `pid`. Phase BC is the explicit redesign to close that gap.

## Testing Strategy

- Hook script unit tests for payload parsing, root/context chaining, and
  fail-open/fail-closed behavior.
- Parity tests between repo-local scripts and installed crate assets.
- Integration tests validating:
  - canonical session-state writes
  - normalized state transitions
  - mandatory structured logging
  - `PreToolUse(Agent)` fenced JSON validation

## Non-Goals

- Hook scripts are not a second state engine.
- Hook scripts do not perform roster reconciliation directly.
- Hook scripts do not bypass daemon authorization rules.
- Hook runtime correctness must not depend on a daemon round-trip.
