# Codex Hook Strategy

## Purpose

Define ATM strategy for Codex lifecycle/activity signaling and hook-equivalent
state transitions.

Codex does not provide Claude-style lifecycle hooks; this document is the source
of truth for how ATM derives equivalent signals.

## Primary References

- [codex-agent-registration.md](codex-agent-registration.md)
- [codex-json-schema.md](codex-json-schema.md)
- [codex-tmux-adapter.md](codex-tmux-adapter.md)
- [requirements.md](requirements.md)

## Strategy Goals

- Keep daemon as single source of truth for team-member state.
- Produce deterministic Codex lifecycle signals without relying on Claude hooks.
- Preserve consistent semantics with Claude/Gemini strategy documents.

## Hook-Equivalent Signal Sources

| Source | ATM Role | Strategy |
| --- | --- | --- |
| `atm` CLI (`send`, `read`, `register`) | identity/session/PID correlation | Use as primary registration and heartbeat path |
| Codex JSON stream (`idle`, `done`) | activity transitions | Map to idle/terminal transitions through daemon event path |
| Process liveness checks | dead-state confirmation | PID death is authoritative dead signal |

## Lifecycle Mapping Strategy

Codex integration should emit unified daemon `hook-event` envelopes with
`source.kind = "agent_hook"` (or `atm_mcp` where proxy-owned):

- startup/registration -> `session_start`
- turn idle (`idle`) -> `notification_idle_prompt` or `teammate_idle` (adapter-defined stable mapping)
- turn complete (`done`) -> `stop`
- explicit shutdown -> `session_end`

## Identity, Session, and PID Strategy

- Team identity resolution priority:
  1. explicit CLI flags
  2. environment (`ATM_TEAM`, `ATM_IDENTITY`)
  3. repo config (`.atm.toml`)
- PID attached to state/log events must represent the long-lived Codex process.
- Session IDs must be stable for a process lifetime; role resume creates a new
  process/session identity pair.

## Behavior Rules

- No inferred liveness from activity flags (`isActive` means busy, not alive).
- Missing hook-equivalent events must not silently delete active members.
- Cleanup/removal decisions must use daemon state + PID liveness checks.

## Installation Strategy

- `atm init` installs Codex adapter wiring when Codex is available.
- Installed behavior must remain parity-tested with repo-local behavior.

## Testing Strategy

- Unit tests for identity resolution and PID/session assignment.
- Integration tests for lifecycle mapping (`idle`, `done`, shutdown).
- Multi-agent tests validating correct online/idle/busy/dead transitions.
