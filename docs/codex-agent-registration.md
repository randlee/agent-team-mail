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

## arch-ctm Investigation Findings (2026-03-06)

arch-ctm investigated the root cause directly and found a more specific issue than the original analysis: the `backend_expected_rule` in `send.rs` matched Codex only when `comm=="codex"` and ignored the legacy `agentType` fallback. PPID traversal could not register a sender hint/session because the backend rule never fired.

**arch-ctm's fixes (Option B implementation)**:
1. `backend_expected_rule` now honors legacy `agentType=codex/gemini`
2. Process matcher normalizes basename for full-path executables
3. PPID traversal depth increased 8→16
4. Non-hook session ID stabilized to `local:<agent>:pid:<pid>` on first send event
5. Log format: removed pid/ppid prefix noise from send lines; only sender/recipient PID slots shown

**Result**: `atm logs` now shows `arch-ctm@atm-dev [27068]` after first send event.

**Remaining gap** (arch-ctm confirmed): `atm doctor` still emits `ACTIVE_WITHOUT_SESSION` because the session_registry in `socket.rs` is only populated via `session_start` hooks — the send-event path does not write to it. This is a separate doctor/session-query reconciliation issue.

**Comparison vs. original Option A/B**:

| | arch-ctm (Option B enhanced) | Option A (`atm register`) |
|--|--|--|
| `atm logs` PID | Yes — via PPID traversal on send | Yes — via explicit register call |
| `atm doctor` session record | No — ACTIVE_WITHOUT_SESSION remains | Yes — explicit session_registry entry |
| `atm cleanup` safety | No — still removed if no state record | Yes — explicit state record |
| Launch script change | No | Yes |
| Works without sending first | No | Yes |

## Recommendation

**AC.2 sprint**: Accept arch-ctm's Option B fixes for PID display (already implemented). Add **Option C** (cleanup guard for external agents) as a safety net, since arch-ctm's fix does not populate the session_registry and cleanup still treats no-state as stale.

Option A (`atm register`) can be deferred: once Option B+C are stable, Option A adds proper session tracking and removes the `ACTIVE_WITHOUT_SESSION` doctor noise.

Option D (registry persistence) deferred until A+C are stable.

---

## Files to Modify

### AC.2a — Option B fixes (arch-ctm, already implemented)
| File | Change |
|------|--------|
| `crates/atm/src/commands/send.rs` | Fix `backend_expected_rule` for legacy agentType codex/gemini |
| `crates/atm/src/commands/send.rs` | Normalize basename in process matcher |
| `crates/atm/src/commands/send.rs` | PPID traversal depth 8→16 |
| `crates/atm/src/commands/send.rs` | Synthetic session ID `local:<agent>:pid:<pid>` |
| `crates/atm-core/src/log_reader.rs` | Log format: remove pid/ppid noise from send lines |

### AC.2b — Option C cleanup guard
| File | Change |
|------|--------|
| `crates/atm-daemon/src/daemon/cleanup.rs` | Skip removal of `agentType` in `{"codex","gemini","external"}` unless `last_seen` > 7 days |

### AC.2c — Option A (`atm register`) — deferred
| File | Change |
|------|--------|
| `crates/atm-daemon/src/daemon/socket.rs` | Add `register` socket command handler |
| `crates/atm-core/src/socket_protocol.rs` | Add `Register` request variant |
| `crates/atm/src/commands/register.rs` | New `atm register` subcommand |
| `crates/atm/src/main.rs` | Wire register subcommand |
| `scripts/launch-worker.sh` | Add `atm register` call after Codex startup |

**Test coverage needed (AC.2b):**
- `test_cleanup_does_not_remove_external_agent_without_state`
- `test_cleanup_removes_external_agent_after_long_absence`

**Test coverage needed (AC.2a, arch-ctm):**
- Tests for `backend_expected_rule` with `agentType=codex`
- Tests for PPID traversal finding Codex process

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
