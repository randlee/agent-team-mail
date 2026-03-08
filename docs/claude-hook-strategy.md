# Claude Hook Strategy

## Purpose

Define the Claude-side hook strategy for ATM lifecycle/state tracking, spawn
policy enforcement, and identity correlation.

This document is the source of truth for Claude hook behavior and mapping.
`docs/requirements.md` should keep only stable invariants and reference this
document for hook-specific flow details.

## Primary References

- [agent-teams-hooks.md](agent-teams-hooks.md)
- [requirements.md](requirements.md)
- [settings.json](../.claude/settings.json)
- [scripts/](../.claude/scripts/)

## Strategy Goals

- Keep daemon state as the single source of truth.
- Use hooks for fast lifecycle/activity updates.
- Fail-open for observability hooks; fail-closed only for policy gates.
- Keep local scripts and installed scripts behaviorally identical.

## Hook Surface and Responsibility

| Claude Hook | ATM Purpose | Strategy |
| --- | --- | --- |
| `SessionStart` | session registration + session file write + context bootstrap | Emit lifecycle event and refresh session mapping |
| `SessionEnd` | explicit session termination signal | Emit lifecycle event and cleanup session file |
| `PreToolUse(Task)` | spawn policy enforcement | Block unauthorized/unsafe spawn calls with actionable errors |
| `PreToolUse(Bash)` | CLI identity correlation setup | Write per-invocation identity context for `atm` CLI |
| `PostToolUse(Bash)` | cleanup identity artifacts | Remove per-invocation identity context |
| `TeammateIdle` | low-latency idle transition | Relay idle event to daemon |
| `PermissionRequest` | blocked-on-approval activity state | Relay blocked-permission event to daemon |
| `Stop` | turn-complete activity transition | Relay turn-stop to daemon as idle signal |
| `Notification(idle_prompt)` | periodic idle heartbeat | Relay idle heartbeat for convergence |

## Lifecycle Mapping Strategy

Claude relays emit one unified daemon envelope through `hook-event` with
`source.kind = "claude_hook"`.

Preferred event mapping:

- `SessionStart` -> `session_start`
- `SessionEnd` -> `session_end`
- `TeammateIdle` -> `teammate_idle`
- `PermissionRequest` -> `permission_request`
- `Stop` -> `stop`
- `Notification(idle_prompt)` -> `notification_idle_prompt`

## Deployment Strategy

- Project hooks in `.claude/settings.json` for repo-specific policy and relay behavior.
- Global hooks in `~/.claude/settings.json` for lifecycle events that must follow every Claude session.
- `atm init` is responsible for installing/updating hook registrations and scripts.

## Behavior Rules

- Spawn-gate hook is fail-closed (`exit 2` on policy violation).
- Relay hooks are fail-open (`exit 0` on errors).
- Hook-reported PID must represent the long-lived session process (`os.getppid()` in hook subprocess).
- Hook logic must not infer liveness from activity fields (`isActive` means busy, not alive).

## State and Data Ownership

- Daemon owns live state transitions and liveness decisions.
- Hooks provide event signals and identity/session metadata.
- Session file data supports CLI identity resolution when shell env lacks session metadata.

## Testing Strategy

- Hook script unit tests for payload parsing, team/identity resolution, and fail-open behavior.
- Parity tests between repo-local scripts and installed crate assets.
- Integration tests validating daemon state transitions from emitted hook events.

## Non-Goals

- Hook scripts are not a second state engine.
- Hook scripts do not perform roster reconciliation directly.
- Hook scripts do not bypass daemon authorization rules.
