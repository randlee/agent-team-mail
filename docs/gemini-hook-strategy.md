# Gemini Hook Strategy

## Purpose

Define ATM integration strategy for Gemini CLI hooks after understanding Gemini's
hook runtime behavior.

This document is the source of truth for Gemini hook integration strategy in
ATM. Raw Gemini hook runtime details are documented in
`docs/gemini-hooks.md`.

## Primary References

- [gemini-hooks.md](gemini-hooks.md)
- [index.md](../gemini-cli/docs/hooks/index.md)
- [reference.md](../gemini-cli/docs/hooks/reference.md)
- [hooks/](../gemini-cli/packages/core/src/hooks/)

## Strategy Goals

- Reuse Gemini-native hook points for ATM lifecycle/activity updates.
- Preserve daemon single source of truth for state.
- Keep hook relay scripts non-blocking for ATM observability use cases.
- Avoid high-volume/noisy events that degrade performance.

## Minimum Viable Hook Set (ATM)

| Gemini Hook | ATM Value | Strategy |
| --- | --- | --- |
| `SessionStart` | session registration | Required |
| `SessionEnd` | session termination signal | Required |
| `BeforeAgent` | active/busy transition | Recommended |
| `AfterAgent` | idle transition | Recommended |
| `Notification` | permission/attention signal | Optional (policy-dependent) |
| `BeforeTool` / `AfterTool` | richer observability | Optional |

`BeforeModel` / `AfterModel` are intentionally excluded from default ATM relay
to avoid chunk-level event volume and daemon noise.

## Unified Event Envelope Strategy

Gemini relay scripts emit the same daemon `hook-event` envelope used by Claude:

- `event`
- `team`
- `agent`
- `session_id`
- `process_id`
- `source.kind = "agent_hook"`
- optional event-specific payload fields

## Identity and Team Resolution Strategy

Resolution order for relay scripts:

1. hook payload fields (if present)
2. environment (`ATM_TEAM`, `ATM_IDENTITY`)
3. repo config (`.atm.toml` `[core]`)

If required routing values are unavailable, relay exits `0` and emits nothing.

## PID and Session Strategy

- `process_id` should resolve to the long-lived Gemini process (`os.getppid()` in command hook subprocess).
- `session_id` should come from Gemini hook payload (`session_id`).
- Daemon remains authoritative for liveness checks and stale-PID handling.

### Session JSONL Log Location and Naming

Observed runtime log location (validated in dogfooding):

- Base directory: `~/.claude/projects/`
- Workspace folder naming: absolute project path with `/` mapped to `-`
  (example: `/Users/randlee/Documents/github/agent-team-mail` ->
  `-Users-randlee-Documents-github-agent-team-mail`)
- Session file naming: `<session_id>.jsonl`

Example:

- `/Users/randlee/.claude/projects/-Users-randlee-Documents-github-agent-team-mail/09ab30b4-a04f-42cf-a8a8-de1434bec38c.jsonl`

Operational rule:

- `atm doctor` session identifiers for active Gemini/Claude-managed sessions
  must map to this JSONL filename convention (`session_id` == JSONL basename
  without `.jsonl`) for the same workspace.

## Hook Outcome Policy for ATM Relays

- ATM relay hooks should return success (`exit 0`) even on relay errors.
- ATM relays should not block Gemini model/tool flow.
- Policy enforcement logic (if needed) should live in dedicated policy hooks, not state relay hooks.

## Installation Strategy

- `atm init` should install/update Gemini hook configuration when Gemini is installed.
- Installed hook commands must use stable path resolution and work from any cwd.
- Repo-local and installed hook behavior must remain parity-tested.

## Testing Strategy

- Unit tests for team/identity/session extraction and payload formation.
- Integration tests for daemon state transitions from Gemini hook events.
- Parity tests for local vs installed script roots.
- Volume tests ensuring disabled/default event set does not spam daemon logs.

## Open Decisions

- Whether `Notification` should always map to `permission_request`-style state
  or be informational only.
- Whether `BeforeTool`/`AfterTool` relay should be enabled by default or gated
  behind a verbosity/config flag.
