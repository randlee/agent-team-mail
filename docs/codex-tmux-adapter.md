# Codex TMUX Worker Adapter — Design Outline

## Goal
Enable async Codex teammates that can receive inbox messages and respond without a foreground terminal. The user can attach to a tmux pane to observe or intervene.

## Components
1. **Daemon plugin (adapter, generic)**
   - Watches inbox events for configured agents.
   - Routes message payloads to the tmux worker.
   - Writes worker responses back to the sender inbox.

2. **TMUX worker (backend-specific)**
   - Runs Codex in its own tmux pane (first backend).
   - Receives messages via `tmux send-keys`.
   - Emits responses to stdout, which the adapter captures.

## Data Flow
1. Inbox event → plugin matches agent subscription.
2. Plugin formats prompt and sends via `tmux send-keys -t <pane> ... Enter`.
3. Codex produces response.
4. Adapter captures output (prefer log file tailing; capture-pane has limits).
5. Adapter writes response to inbox (as `from = <agent>`).

## Safety
- No stdin injection into the user’s active terminal.
- Each agent has its own tmux pane.
- Explicit enable/disable via daemon config.

## Configuration (draft)
Machine-level `~/.config/atm/daemon.toml`:
```toml
[workers.codex]
enabled = true
tmux_session = "atm-workers"
default_agent = "arch-ctm@atm-planning"
```

Repo-level `./.atm/config.toml` (per repo):
```toml
[plugins.ci_monitor]
enabled = true

[workers.codex.agents]
"arch-ctm@atm-planning" = { enabled = true }
```

## Open Questions
- How to reliably capture worker output (prefer log file; evaluate PTY if needed).
- Whether to allow multiple concurrent requests to the same agent.
- How to surface worker health (crash/restart behavior).
