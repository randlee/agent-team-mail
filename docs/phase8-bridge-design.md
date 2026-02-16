# Phase 8 Bridge Plugin Design

## Purpose
The Bridge plugin synchronizes agent inbox queues across machines. It does **not** orchestrate work; it simply keeps per-agent inbox JSON files in sync and relies on `message_id` + timestamps for deduplication.

## Decided Architecture

### 1) Naming
- **Primary format**: `agent@team.hostname`.
- `hostname` is optional and defaults to local.
- Aliases are supported (e.g., `PM-MAC` → `team-lead@my-project.mac-studio`).
- **Conflict detection**: maintain a hostname registry; warn if a team name collides with a hostname.

### 2) Topology
- **Hub-spoke**.
- All queues are duplicated across machines.
- Hub is the synchronization point (not a router).

### 3) Transport (MVP)
- **SSH/SFTP** with key-based auth.
- Persistent connections via `ControlMaster`.
- Periodic sync with backoff to handle intermittent connectivity.

### 4) Inbox File Naming
- **Per-origin files** to avoid cross-machine write conflicts:
  - `~/.claude/teams/<team>/inboxes/<agent>.<hostname>.json`
- Each machine only writes its own origin files.
- Bridge syncs these across machines.
- **Read path** merges all origins for an agent in memory, sorted by timestamp.
- **Dedup** by `message_id`.

### 5) Bridge Plugin Responsibilities
- Watch local inbox files for changes.
- Push new messages to remote(s) via SSH/SFTP.
- Receive incoming files from remote(s) and write locally.
- Deduplicate by `message_id`.

### 6) Team Config
- **Hub is source of truth**.
- Agent list owned by local team-lead.
- No cross-team authority to manage remote agents.
- Config is synced so all machines know who exists.

### 7) Auth (MVP)
- Minimal security assumptions (secure LAN/VPN).
- SSH key-based authentication.

### 8) Non-Goals
- Routing messages.
- Managing plans/tasks.
- Spinning up remote agents.
- Coordinating git merges.

### 9) Communication Patterns
- Mostly team-lead ↔ team-lead.
- Dev/QA cross-talk only for specific debugging.

### 10) Deferred
- Per-origin → single canonical merge optimization.
- HTTP/WebSocket transport.
- Advanced auth (tokens, MFA, role policies).

---

## Implementation Notes

### File Merge Strategy
- On read, combine all `agent.<hostname>.json` files.
- Sort by `timestamp` (stable tie-breaker: `message_id`, then origin filename).
- Dedup by `message_id`.

### Conflict Avoidance
- Never write to a file owned by another host.
- All writes are local-only; synchronization is file-copy based.

---

## Sprint Decomposition (Proposed)

### Sprint 8.1 — Bridge Core + Config
- Bridge plugin scaffold (init/run/shutdown).
- SSH/SFTP transport wrapper with ControlMaster support.
- Config parsing for bridge targets, hostnames, aliases.
- File ownership rules for per-origin inbox files.

**Files**
- `crates/atm-daemon/src/plugins/bridge/`
- `crates/atm-core/src/config/` (bridge config structs)
- `docs/phase8-bridge-design.md`

### Sprint 8.2 — Sync Engine + Dedup
- Local watcher integration.
- Push/pull cycle with backoff.
- Dedup by `message_id`.
- Merge view helper for read paths.

**Files**
- `crates/atm-daemon/src/plugins/bridge/`
- `crates/atm-core/src/schema/` (helper for merge view)
- `crates/atm-daemon/src/daemon/event_loop.rs` (if needed)

### Sprint 8.3 — Team Config Sync + Hardening
- Sync team config from hub to spokes.
- Conflict warnings (hostname registry).
- Logging and metrics.
- Failure handling and retry policy.

**Files**
- `crates/atm-daemon/src/plugins/bridge/`
- `crates/atm-core/src/config/`

### Sprint 8.4 — Tests + Docs
- Integration tests with mock SSH backend.
- Docs + examples for multi-machine setup.
- Ops checklist for VPN/SSH.

**Files**
- `crates/atm-daemon/tests/bridge_*`
- `docs/`

---

## Dependency Graph (High Level)

1. Bridge config + aliases + hostname registry
2. Transport wrapper (SSH/SFTP, ControlMaster)
3. Per-origin file syncing logic
4. Dedup + merged read view
5. Team config sync
6. Tests + docs

---

## Open Questions / Risks

1. **Timestamp consistency** across hosts (clock drift).
   - Mitigation: use `message_id` as primary dedup key; allow small timestamp skew.

2. **Large inboxes**: merge across many origin files can be expensive.
   - Mitigation: periodic compaction or capped retention.

3. **SSH availability on Windows**.
   - Mitigation: document requirement; fallback to Win32 OpenSSH.

4. **Security beyond LAN/VPN**.
   - Deferred to future phase.

5. **Alias collisions**.
   - Mitigation: require unique alias names and warn on duplicates.
