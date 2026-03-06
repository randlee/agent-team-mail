# Codex Agent State Persistence — Root Cause & Design

**Analyst**: team-lead, 2026-03-06
**Issue**: arch-ctm always shows `[-]` (no PID/session) in all ATM logs
**Severity**: High — causes `atm cleanup` to remove active Codex agents

---

## Symptom

```
atm logs --limit 5:
  [atm/arch-ctm pid=44209/ppid=27068] send arch-ctm@atm-dev [-] -> team-lead@atm-dev [-]
  [atm/team-lead pid=44476/ppid=44474] send team-lead@atm-dev [17033] -> arch-ctm@atm-dev [-]

atm doctor:
  arch-ctm  codex  unknown  Offline  Unknown  -  -
  team-lead ...    ...      Online   Busy     17033  local:team-lead

atm teams cleanup atm-dev:
  → Removes arch-ctm (treats no-state as stale)
```

The `[-]` bracket in the log format is `{daemon-state-pid}` — the PID the daemon has on record for that member. `team-lead` shows `[17033]` because it registered via session_start hook. `arch-ctm` shows `[-]` because it never registered.

---

## Root Cause Analysis

### How daemon state gets populated

The daemon populates member state in `handle_hook_event_command_with_dedup()` in `socket.rs`:

```
session_start hook event
  -> validate PID against BackendRule
  -> session_registry.upsert_for_team(team, agent, session_id, pid)
  -> state_store.set_state(agent, Active)
```

This only fires when a **Claude Code hook** delivers a `session_start` event to the daemon socket.

### Why Codex agents never register

Codex (`arch-ctm`) runs as a standalone CLI process (`codex` binary). It does **not** run inside a Claude Code session and therefore **never fires Claude Code lifecycle hooks**. No hook → no `session_start` event → no session record → daemon never learns arch-ctm's PID or session ID.

### Why team-lead stays registered across daemon restarts

The daemon's `session_registry` is in-memory. On restart it's empty. However:
- Claude Code fires `session_start` on every context compaction (via the global `~/.claude/settings.json` `SessionStart` hook)
- The hook re-registers team-lead within seconds of each compact cycle
- So team-lead's gap is short (a few seconds) and barely visible

### Why `atm cleanup` removes arch-ctm

`atm teams cleanup` checks for "stale" members by cross-referencing:
1. Team config (`config.json`) members list
2. Daemon state records

arch-ctm has an entry in `config.json` but zero daemon state records. The cleanup logic treats this absence as staleness and removes arch-ctm.

This is an incorrect assumption: **absence of daemon state is not equivalent to member being offline/stale**. For external agents (Codex, Gemini) that cannot fire hooks, this will always be the case.

### ACTIVE_WITHOUT_SESSION for team-lead (secondary issue)

The doctor also shows `ACTIVE_WITHOUT_SESSION` for team-lead. This fires in `session_start` handling when:
- `member.is_active == Some(true)` in config.json (activity hint from last session)
- But no existing session record in the in-memory registry

This is an expected transient state after daemon restart — it resolves as soon as the compact-triggered `session_start` fires. However, it causes unnecessary WARN noise in doctor output after every restart.

---

## Design Options

### Option A (Primary): `atm register` self-registration command

Add a new socket command (and CLI wrapper) that Codex agents call at startup:

```bash
atm register --session-id <uuid> --pid <pid>
# Or auto-detect: atm register  (daemon uses ppid of the request process)
```

**Daemon side** (`socket.rs`): New `register` handler that:
1. Validates the agent is a known team member
2. Skips PID backend validation (external agents are not Claude processes)
3. Calls `session_registry.upsert_for_team()` + `state_store.set_state(Active)`
4. Emits the same session identity change events as `session_start`

**CLI side** (`atm register`): Simple command that sends register event to daemon. Reads identity from ATM_IDENTITY env / .atm.toml pipeline.

**Startup integration** (`launch-worker.sh`): Add `atm register` call after Codex launch, once the process is confirmed running.

**Benefits**: Clean registration, proper PID tracking, session identity visible in logs.

### Option B: Activity-based inference

When daemon receives a send/recv log event from an agent with no session record, auto-infer as "active" and use the event's `ppid` as PID.

**Pros**: Zero changes to Codex startup.
**Cons**: Only fires when arch-ctm sends a message; `atm doctor` still shows Offline until first message; ppid may be a shell wrapper, not the actual Codex process; fragile.

### Option C (Band-aid): `atm cleanup` guard for external agents

Change `atm cleanup` to skip removal of members with `agentType` in `{"codex", "gemini", "external"}` unless they have been absent from `state.json`'s `last_seen` for more than N days (e.g., 7 days).

**Pros**: Prevents the catastrophic "remove active arch-ctm" failure. Quick fix.
**Cons**: Does not fix the `[-]` display or session tracking. Cleanup becomes imprecise for external agents.

### Option D: Session registry persistence

Persist `session_registry` to disk (e.g., `~/.config/atm/session-registry.json`), reload on daemon startup.

**Pros**: Fixes the "after restart" gap for all agent types.
**Cons**: Stale records if a session actually died while daemon was down. Needs TTL logic. Codex still shows `[-]` on first startup.

---

## Recommendation

**AC.2 sprint**: Implement **Option A** (self-registration) + **Option C** (cleanup guard) together.

Option A gives correct behavior. Option C is a safety net that prevents the worst outcome (cleanup removing active agents) even if Option A registration is missed.

Option D (registry persistence) is useful but can be deferred to a follow-up sprint once A+C are stable.

---

## Files to Modify

| File | Change |
|------|--------|
| `crates/atm-daemon/src/daemon/socket.rs` | Add `register` socket command handler |
| `crates/atm-core/src/socket_protocol.rs` | Add `Register` request variant (or reuse hook_event infrastructure) |
| `crates/atm/src/commands/register.rs` | New `atm register` subcommand |
| `crates/atm/src/main.rs` | Wire register subcommand |
| `crates/atm-daemon/src/daemon/cleanup.rs` | Add `agentType` guard for external agents |
| `scripts/launch-worker.sh` | Add `atm register` call after Codex startup |

**Test coverage needed:**
- `test_register_codex_agent_sets_session_record`
- `test_register_sets_state_active`
- `test_cleanup_does_not_remove_external_agent_without_state`
- `test_cleanup_removes_external_agent_after_long_absence`

---

## Observed Behavior Log

From `atm logs --limit 20` (2026-03-06):
```
arch-ctm pid=43387/ppid=27068  ->  arch-ctm@atm-dev [-]   (ppid=27068 is arch-ctm tmux session)
team-lead pid=49084/ppid=49082  ->  team-lead@atm-dev [17033]
```

After daemon restart (22:26:31):
```
daemon_autostart_success
team-lead send  ->  arch-ctm@atm-dev [-]   (arch-ctm still [-] even after daemon restart + autostart)
```

`registry.json` at `~/.config/atm/agent-sessions/atm-dev/`:
```json
{"version": 1, "sessions": []}   // Always empty — no Codex registration ever fires
```
